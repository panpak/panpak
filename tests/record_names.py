#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A package of one app cannot replace the record of another app (PP-01), with real binaries.

The name of a record in `<store>/packages/` was `<id>-<version>-<first 12 hex of the root>.rec`: `org.a-1` at version `2` and `org.a` at version `1-2` gave one name, and the
root is the hash of the index, which does not hold the manifest, so a hostile package made of the same files as a public one has the same root. Installing it replaced the
record that the other app's `current` names, and the start of that app ran with the manifest of the hostile package (its permissions, arguments and environment), under a pin
that the hostile package had made for itself.

Checked: the two packages have one root; after both are installed each app has its own record; the victim still starts clean; and a record that is put under another
record's name (a copy of the hostile record in the place of the victim's) is refused at the start, because the name of a record says whose it is.
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
from incremental import BIN  # noqa: E402

VICTIM = ('org.a-1', '2')
ATTACKER = ('org.a', '1-2')


def run(args, env, ok=True, timeout=60):
    p = subprocess.run([str(x) for x in args], env=env, capture_output=True, text=True, timeout=timeout)
    if ok:
        assert p.returncode == 0, p.stdout + p.stderr
    return p


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-recnames-') as tmp:
        tmp = Path(tmp)
        isolation.require_isolated_home(tmp, tmp)
        runtime = tmp / 'run'
        runtime.mkdir(mode=0o700)
        env = dict(os.environ, HOME=str(tmp), XDG_DATA_HOME=str(tmp / 'data'), XDG_CACHE_HOME=str(tmp / 'cache'), XDG_CONFIG_HOME=str(tmp / 'config'),
                   XDG_RUNTIME_DIR=str(runtime), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
        env.pop('WAYLAND_DISPLAY', None)
        packages = {}
        for who, (app_id, version) in (('victim', VICTIM), ('attacker', ATTACKER)):
            key = tmp / f'{who}.key'
            out = run([BIN / 'panpak', 'keygen', '-o', key], env).stdout
            pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
            tree = tmp / f'tree-{who}'
            tree.mkdir()
            (tree / 'hello').write_text('#!/bin/sh\necho "${PWNED:-clean}"\n')
            (tree / 'hello').chmod(0o755)
            (tree / 'data.bin').write_bytes(bytes(range(256)) * 400)
            extra = '[launch.env]\nPWNED = "hostile-manifest"\n' if who == 'attacker' else ''
            (tree / 'panpak.toml').write_text(f'[app]\nid="{app_id}"\nname="T"\nversion="{version}"\nentry="hello"\n[update]\nkey="{pub}"\n{extra}')
            out_file = tmp / f'{who}.panpak'
            run([BIN / 'panpak', 'pack', tree, '--stub', BIN / 'panpak-stub', '--secret-file', key, '-o', out_file], env)
            packages[who] = out_file
        roots = {w: p.read_bytes()[-32:].hex() for w, p in packages.items()}
        assert roots['victim'] == roots['attacker'], 'the two packages hold the same files, so they have one root'
        store = tmp / 'data/panpak'
        stub = store / 'bin/panpak-stub'
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', packages['victim']], env)
        assert run([stub, 'run', store / 'apps' / VICTIM[0] / 'current'], env).stdout.strip() == 'clean'
        victim_record = (store / 'apps' / VICTIM[0] / 'current').resolve()
        before = victim_record.read_bytes()
        run([BIN / 'panpak', 'install', '--yes', '--trust-first-key', '--runtime', BIN / 'panpak-stub', packages['attacker']], env)
        attacker_record = (store / 'apps' / ATTACKER[0] / 'current').resolve()
        assert victim_record.read_bytes() == before, 'installing the other package did not change the record of the first app'
        assert victim_record != attacker_record, 'each app has a record of its own'
        p = run([stub, 'run', store / 'apps' / VICTIM[0] / 'current'], env)
        assert p.stdout.strip() == 'clean', f'the first app runs with its own manifest: {p.stdout}{p.stderr}'
        assert run([stub, 'run', store / 'apps' / ATTACKER[0] / 'current'], env).stdout.strip() == 'hostile-manifest'
        print('two apps whose id and version make the same text, and one root: two records, each app starts with its own manifest PASS')

        # A record put under the name of another one (the hostile record in the place of the victim's) is refused: the name says whose it is.
        for entry in (store / 'packages').iterdir():
            entry.chmod(0o644)
        shutil.copyfile(attacker_record, victim_record)
        p = run([stub, 'run', store / 'apps' / VICTIM[0] / 'current'], env, ok=False)
        assert p.returncode != 0 and 'hostile-manifest' not in p.stdout and 'does not match' in p.stderr, p.stdout + p.stderr
        # The link of one app that names the record of another is refused too (the pin is looked up under the id of the app that was started).
        (store / 'apps' / VICTIM[0] / 'current').unlink()
        (store / 'apps' / VICTIM[0] / 'current').symlink_to(os.path.relpath(attacker_record, store / 'apps' / VICTIM[0]))
        p = run([stub, 'run', store / 'apps' / VICTIM[0] / 'current'], env, ok=False)
        assert p.returncode != 0 and 'hostile-manifest' not in p.stdout and 'another app' in p.stderr, p.stdout + p.stderr
        print('a record under the name of another, and the link of one app to the record of another, are refused at the start PASS')
        subprocess.run(['chmod', '-R', 'u+w', str(tmp)], check=False)


if __name__ == '__main__':
    main()
