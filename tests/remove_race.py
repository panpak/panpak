#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""A start that comes in while `panpak remove` takes the app away, and a remove while a copy runs.

Usage: python3 tests/remove_race.py [work-directory]
Build first: `cargo build --release --target x86_64-unknown-linux-musl` (the real stub and launcher) and
`cargo build -p panpak` (the debug `panpak remove`, which has the test hook PANPAK_TEST_PAUSE_MS that makes a
removal slow). Everything runs in an isolated HOME inside the work directory (tests/isolation.py).

1. `remove` holds the lock of the copies and waits; the launcher is started meanwhile: it must wait, and once the
   app is gone it must end with PANPAK-E2004 without running the app.
2. A copy that is running holds the lock: `remove` must refuse with PANPAK-E2002 and touch nothing; after the copy
   ended, `remove` works.
"""
import fcntl
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REL = ROOT / 'target/x86_64-unknown-linux-musl/release'
DEBUG_PANPAK = ROOT / 'target/debug/panpak'
sys.path.insert(0, str(ROOT / 'tests'))
import isolation  # noqa: E402

ID = 'org.example.race'
APP = '''#!/bin/sh
echo started > "$XDG_DATA_HOME/ran"
case "$1" in --hold) sleep 60;; esac
'''


def need(cond, msg):
    if not cond:
        print('FAIL:', msg)
        sys.exit(1)


def main():
    for b in (REL / 'panpak', REL / 'panpak-stub', DEBUG_PANPAK):
        need(b.exists(), f'{b} is missing: build first (see the top of this file)')
    work = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / 'target/remove-race'
    shutil.rmtree(work, ignore_errors=True)
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True)
    runtime = Path(isolation.owned_mkdtemp('race-rt-'))
    env = {'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_RUNTIME_DIR': str(runtime)}
    for k in ('WAYLAND_DISPLAY', 'DISPLAY', 'LANG'):
        if k in os.environ:
            env[k] = os.environ[k]

    def run(binary, *args, **kw):
        return subprocess.run([str(binary), *map(str, args)], env={**env, **kw.pop('extra', {})}, capture_output=True, text=True, timeout=120, **kw)

    key = work / 'secret.key'
    out = run(REL / 'panpak', 'keygen', '-o', key).stdout
    pub = next(line.split()[-1] for line in out.splitlines() if line.startswith('key:'))
    tree = work / 'tree'
    (tree / 'bin').mkdir(parents=True)
    (tree / 'bin/app').write_text(APP)
    (tree / 'bin/app').chmod(0o755)
    (tree / 'panpak.toml').write_text(f'[app]\nid = "{ID}"\nname = "Race"\nversion = "1"\nentry = "bin/app"\n[update]\nkey = "{pub}"\n')
    pkg = work / 'race.panpak'
    p = run(REL / 'panpak', 'pack', tree, '--stub', REL / 'panpak-stub', '--secret-file', key, '-o', pkg)
    need(p.returncode == 0, p.stderr)
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    launcher = home / '.local/bin' / ID
    ran = home / f'.local/share/panpak/userdata/{ID}/data/ran'
    lockfile = runtime / 'panpak' / ID / 'lock'

    # --- 1. a start during a slow removal
    slow = subprocess.Popen([str(DEBUG_PANPAK), 'remove', ID], env={**env, 'PANPAK_TEST_PAUSE_MS': '3000'}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    held = False
    for _ in range(100):
        if lockfile.exists():
            f = open(lockfile)
            try:
                fcntl.flock(f, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                held = True
            finally:
                f.close()
            if held:
                break
        time.sleep(0.05)
    need(held, 'the removal did not take the lock')
    t0 = time.time()
    start = subprocess.Popen([str(launcher)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    time.sleep(1.0)
    need(start.poll() is None, f'the start did not wait for the removal: {start.communicate()}')
    sout, serr = slow.communicate(timeout=60)
    need(slow.returncode == 0, f'the removal failed: {sout}{serr}')
    o, e = start.communicate(timeout=60)
    need(start.returncode != 0 and 'PANPAK-E2004' in e and 'removed' in e, f'the start ended wrongly ({start.returncode}): {o}{e}')
    need(not ran.exists(), 'the app ran although it was removed')
    need(time.time() - t0 >= 1.5, 'the start did not wait')
    print(f'start during a removal: waited, then ended with PANPAK-E2004 ({e.strip()[:90]}); the app did not run: ok')

    # --- 2. a running copy stops the removal
    p = run(REL / 'panpak', 'install', '--yes', '--runtime', REL / 'panpak-stub', pkg)
    need(p.returncode == 0, p.stdout + p.stderr)
    app = subprocess.Popen([str(launcher), '--hold'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    for _ in range(200):
        if ran.exists():
            break
        time.sleep(0.05)
    need(ran.exists() and app.poll() is None, 'the app did not start')
    p = run(DEBUG_PANPAK, 'remove', ID)
    need(p.returncode != 0 and 'PANPAK-E2002' in p.stdout + p.stderr, f'remove did not refuse: {p.stdout}{p.stderr}')
    need(launcher.exists() and (home / f'.local/share/panpak/apps/{ID}/current').exists(), 'remove touched the app although a copy ran')
    app.terminate()
    app.communicate(timeout=30)
    for _ in range(100):
        p = run(DEBUG_PANPAK, 'remove', ID)
        if p.returncode == 0:
            break
        time.sleep(0.1)
    need(p.returncode == 0, f'remove after the copy ended: {p.stdout}{p.stderr}')
    need(not launcher.exists(), 'the launcher is still there')
    print('remove while a copy runs: refused with PANPAK-E2002, touched nothing; after the copy ended it worked: ok')
    isolation.remove_owned(runtime, work)
    shutil.rmtree(work, ignore_errors=True)
    print('all remove race tests passed')


if __name__ == '__main__':
    main()
