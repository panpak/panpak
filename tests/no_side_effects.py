#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""`--help`, `--version` and a wrong command have no effects (the personal key is made only when it is needed to sign), with real binaries.

`panpak convert --help` read `--help` as the AppImage, went on to make the personal key (`~/.config/panpak/secret.key`) and only then said that `--id` was missing. A help
request, a version request, a command that does not exist and a command whose arguments are not complete must not write anything: the key is made after the arguments are
checked, when a package is about to be signed.

The home is a new folder, and so are the XDG folders. After each command the whole home must be empty. `--help` and `--version` must also succeed and print something.
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

# (arguments, must succeed and print something)
CASES = [
    (['--help'], True),
    (['-h'], True),
    (['--version'], True),
    (['convert', '--help'], True),
    (['pack', '--help'], True),
    (['build', '--help'], True),
    (['no-such-command'], False),
    (['convert'], False),
    (['convert', 'app.AppImage'], False),
    (['convert', 'app.AppImage', '--id', 'org.example.x'], False),
    (['convert', 'app.AppImage', '--id', 'org.example.x', '--stub', 'stub'], False),
    (['pack'], False),
    (['pack', '/no/such/folder'], False),
    (['build', '/no/such/folder'], False),
]


def files_below(root):
    return sorted(str(p.relative_to(root)) for p in root.rglob('*'))


def main():
    with tempfile.TemporaryDirectory(prefix='panpak-no-effects-') as tmp:
        tmp = Path(tmp)
        failures = []
        for n, (args, must_work) in enumerate(CASES):
            # A new home for each command, so that one command cannot hide what another left.
            home = tmp / f'home{n}'
            home.mkdir()
            isolation.require_isolated_home(home, tmp)
            runtime = tmp / f'run{n}'
            runtime.mkdir(mode=0o700)
            env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / 'data'), XDG_CACHE_HOME=str(home / 'cache'), XDG_CONFIG_HOME=str(home / 'config'),
                       XDG_RUNTIME_DIR=str(runtime), TMPDIR=str(tmp), LC_ALL='C', PANPAK_NO_BACKGROUND='1')
            env.pop('PANPAK_SECRET_FILE', None)
            p = subprocess.run([str(BIN / 'panpak'), *args], env=env, capture_output=True, text=True, timeout=60, cwd=tmp)
            left = files_below(home)
            if left:
                failures.append(f'panpak {" ".join(args)}: left {left} ({p.stderr.strip()[:100]})')
            if must_work and (p.returncode != 0 or not p.stdout.strip()):
                failures.append(f'panpak {" ".join(args)}: expected success and an output, got rc={p.returncode} stdout={p.stdout[:60]!r} stderr={p.stderr[:80]!r}')
        assert not failures, 'FAILED:\n' + '\n'.join(failures)
        print(f'{len(CASES)} commands that must not write anything: the home stays empty, help and version answer PASS')


if __name__ == '__main__':
    main()
