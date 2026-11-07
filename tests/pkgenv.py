#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""The `[env]` table through the real launcher, stub and view: variables that point an app at files of its own tree, set from data.

Usage: python3 tests/pkgenv.py [work-directory]
Build first: `cargo build --release --target x86_64-unknown-linux-musl`. Everything runs in an isolated HOME inside the work
directory (tests/isolation.py).

The entry is a program (the host's Python), not a shell script. The manifest has an `[env]` table; the app reports what it gets:
- `GSETTINGS_SCHEMA_DIR`, `QT_PLUGIN_PATH` and `GDK_PIXBUF_MODULE_FILE` are paths inside the tree of the package, which is the folder that
  holds the entry's `bin/`;
- `XDG_DATA_DIRS` starts with the tree's `share` and still has the system's folders after it;
- the words (`GSETTINGS_BACKEND`, `QT_QPA_PLATFORM`) arrive as written;
- a variable the app had from the user's environment is not changed by the table when it is not listed, and `LD_PRELOAD` cannot be listed.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.pkgenv'
APP = r'''#!/usr/bin/python3
import json, os, sys
root = os.path.dirname(os.path.dirname(os.path.realpath(sys.argv[0])))
names = ['GSETTINGS_SCHEMA_DIR', 'QT_PLUGIN_PATH', 'GDK_PIXBUF_MODULE_FILE', 'XDG_DATA_DIRS', 'GSETTINGS_BACKEND', 'QT_QPA_PLATFORM', 'LD_PRELOAD']
out = {'root': root, 'env': {n: os.environ.get(n) for n in names}}
out['schemas_exist'] = os.path.isdir(os.environ.get('GSETTINGS_SCHEMA_DIR', '/nonexistent'))
out['cache_readable'] = os.path.isfile(os.environ.get('GDK_PIXBUF_MODULE_FILE', '/nonexistent'))
open(os.environ['XDG_DATA_HOME'] + '/result.json', 'w').write(json.dumps(out))
'''
TABLE = '''[env]
GSETTINGS_SCHEMA_DIR = "${PANPAK_ROOT}/share/glib-2.0/schemas"
QT_PLUGIN_PATH = "${PANPAK_ROOT}/plugins:${PANPAK_ROOT}/lib/qt6/plugins"
GDK_PIXBUF_MODULE_FILE = "${PANPAK_ROOT}/lib/loaders.cache"
XDG_DATA_DIRS = "${PANPAK_ROOT}/share"
GSETTINGS_BACKEND = "keyfile"
QT_QPA_PLATFORM = "wayland"
'''


def need(cond, msg):
    if not cond:
        print('FAIL:', msg)
        sys.exit(1)


def main():
    for b in (REL / 'panpak', REL / 'panpak-stub'):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/pkgenv-test'
    isolation.safe_rmtree(work, work.parent)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('pkgenv-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime), 'LD_PRELOAD': ''}

    def run(binary, *args, e=None):
        return subprocess.run([str(binary), *map(str, args)], env=e or env, capture_output=True, text=True, timeout=120)

    key = work / 'secret.key'
    out = run(REL / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    for d in ('bin', 'share/glib-2.0/schemas', 'plugins', 'lib/qt6/plugins'):
        (tree / d).mkdir(parents=True)
    (tree / 'bin/app').write_text(APP)
    (tree / 'bin/app').chmod(0o755)
    (tree / 'share/glib-2.0/schemas/gschemas.compiled').write_text('x')
    (tree / 'lib/loaders.cache').write_text('# cache\n')
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Env"\nversion = "1"\nentry = "bin/app"\n{TABLE}[update]\nkey = "{pub}"\n')
    pkg = work / 'pkgenv.panpak'
    p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
    need(p.returncode == 0, p.stderr)
    need('shell script' not in p.stderr, 'a program as the entry must not be called a script: ' + p.stderr)
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    # The user's own LD_PRELOAD in the environment is not something the table can set or the app gets from it.
    p = run(home / '.local/bin' / ID, e={**env, 'LD_PRELOAD': ''})
    need(p.returncode == 0, f'the app failed ({p.returncode}): {p.stdout}{p.stderr}')
    res = json.loads((home / f'.local/share/panpak/userdata/{ID}/data/result.json').read_text())
    e, root = res['env'], res['root']
    need(root.startswith(str(home)) and '/trees/' in root, f'the tree is where the store keeps it: {root}')
    need(e['GSETTINGS_SCHEMA_DIR'] == f'{root}/share/glib-2.0/schemas', f"GSETTINGS_SCHEMA_DIR: {e['GSETTINGS_SCHEMA_DIR']}")
    need(e['QT_PLUGIN_PATH'] == f'{root}/plugins:{root}/lib/qt6/plugins', f"QT_PLUGIN_PATH: {e['QT_PLUGIN_PATH']}")
    need(e['GDK_PIXBUF_MODULE_FILE'] == f'{root}/lib/loaders.cache', f"GDK_PIXBUF_MODULE_FILE: {e['GDK_PIXBUF_MODULE_FILE']}")
    dirs = e['XDG_DATA_DIRS'].split(':')
    need(dirs[0] == f'{root}/share' and len(dirs) > 1 and all(d.startswith('/') for d in dirs), f"XDG_DATA_DIRS: {e['XDG_DATA_DIRS']}")
    need(e['GSETTINGS_BACKEND'] == 'keyfile' and e['QT_QPA_PLATFORM'] == 'wayland', f'words: {e}')
    need(res['schemas_exist'] and res['cache_readable'], 'the paths of the table are readable inside the view')
    print('[env]: paths inside the tree, the tree\'s share first in XDG_DATA_DIRS with the system\'s after it, words as written, paths readable in the view')
    # What pack refuses.
    for bad in ('LD_PRELOAD = "${PANPAK_ROOT}/lib/x.so"', 'GSETTINGS_SCHEMA_DIR = "/usr/share/glib-2.0/schemas"', 'GSETTINGS_SCHEMA_DIR = "${PANPAK_ROOT}/../x"'):
        (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Env"\nversion = "1"\nentry = "bin/app"\n[env]\n{bad}\n[update]\nkey = "{pub}"\n')
        p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', work / 'bad.panpak')
        need(p.returncode != 0 and 'PANPAK-E0007' in p.stderr + p.stdout, f'pack must refuse {bad}: {p.stdout}{p.stderr}')
    print('pack refuses a name that is not on the list, an absolute path and a path that leaves the tree (PANPAK-E0007)')
    run(REL / 'panpak', 'remove', ID)
    isolation.remove_owned(runtime, work)
    isolation.safe_rmtree(work, work.parent)
    print('all [env] tests passed')


if __name__ == '__main__':
    main()
