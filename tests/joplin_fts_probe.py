#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Deterministic Joplin FTS probe: symlink vs realdir, sandbox vs direct.

Usage: python3 tests/joplin_fts_probe.py [out.json]
Dedicated Xvfb, fresh isolated profiles, never the desktop.
Modes (symlink excluded from the matrix: verified separately in an isolated
run, same outcome as realdirs; flaky in sequence due to slow Electron startup):
  direct-real     direct tree, profile in ~/.config/joplin-desktop (real)
  direct-profile  direct tree, --profile <realdir in appdata>
  sandbox-profile launcher (Landlock), --profile <realdir in appdata>
Per mode: 50 s, then migration log + DB state (version, notes, FTS tables).
Environment variables: PANPAK_PILOT_DIR (original AppImages,
default ~/panpak-bench/pilot), PANPAK_FUSE_LIB (userspace libfuse,
default ~/fuse).
"""
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import struct
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
from pilot_cycle import environment  # noqa: E402
import isolation  # noqa: E402

ORIG = Path(os.environ.get('PANPAK_PILOT_DIR', str(Path.home() / 'panpak-bench/pilot'))) / 'Joplin-3.7.21.AppImage'
FUSE = os.environ.get('PANPAK_FUSE_LIB', str(Path.home() / 'fuse2')) + '/lib/x86_64-linux-gnu'
WAIT = 50


def pkg_root(pkg):
    with pkg.open('rb') as f:
        f.seek(-80, 2)
        return struct.unpack('<8sIIQQQQ32s', f.read(80))[-1].hex()


def db_state(db):
    con = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    try:
        notes = con.execute('select count(*) from notes').fetchone()[0]
    except Exception:
        notes = -1
    version = con.execute('select * from version').fetchall()
    fts = sorted(r[0] for r in con.execute(
        "select name from sqlite_master where type='table' and name like '%fts%'"))
    return {'notes': notes, 'version': [list(map(int, v)) for v in version], 'fts': fts}


def main():
    out = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    home = ROOT / 'target/pilot-current/home'
    work = ROOT / 'target/pilot-current'
    isolation.require_isolated_home(home, work)
    pkg = work / 'packages/joplin.panpak'
    tree = home / '.local/share/panpak/trees' / pkg_root(pkg)
    appdata = home / '.local/share/org.example.joplin'
    xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
    fd = os.open(work / 'display-probe', os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
    server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x900x24'],
                              pass_fds=[fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    results = {}
    try:
        for _ in range(100):
            text = (work / 'display-probe').read_text().strip()
            if text:
                break
            time.sleep(0.1)
        assert text, 'no Xvfb'
        display = ':' + text

        def fresh_real(path):
            link = home / '.config/joplin-desktop'
            if link.is_symlink() or link.is_file():
                isolation.safe_unlink(link, work)
            elif link.is_dir():
                isolation.safe_rmtree(link, work, ignore_errors=False)
            if path.exists():
                isolation.safe_rmtree(path, work, ignore_errors=False)
            path.mkdir(parents=True)
            return path

        cfgs = {}
        cfgs['direct-real'] = ('direct', fresh_real(home / '.config/joplin-desktop'), [])
        prof1 = appdata / 'probe-direct-profile'
        cfgs['direct-profile'] = ('direct', fresh_real(prof1), ['--profile', str(prof1)])
        prof2 = appdata / 'probe-sandbox-profile'
        cfgs['sandbox-profile'] = ('sandbox', fresh_real(prof2), ['--profile', str(prof2)])

        for name, (mode, profdir, extra) in cfgs.items():
            # Never two overlapping instances: leftovers would skew the probe.
            # -x on the executable name: does not match this script itself.
            left = subprocess.run(['pgrep', '-x', 'joplin'], capture_output=True, text=True)
            assert left.stdout.strip() == '', f'leftover Joplin processes before {name}:\n{left.stdout}'
            env = environment(home, 'joplin', display)
            # environment() recreates the adapter symlink: real dirs needed here
            link = home / '.config/joplin-desktop'
            if link.is_symlink():
                isolation.safe_unlink(link, work)
            argv = ([home / '.local/bin/org.example.joplin'] if mode == 'sandbox'
                    else [tree / 'joplin']) + extra + ['--no-sandbox', '--disable-gpu']
            logf = open(work / f'probe-{name}.log', 'w')
            p = subprocess.Popen([str(a) for a in argv],
                                 env={k: str(v) for k, v in env.items()},
                                 stdout=logf, stderr=logf,
                                 start_new_session=True, cwd=str(appdata))
            time.sleep(WAIT)
            alive = p.poll() is None
            try:
                os.killpg(p.pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
            p.wait(timeout=10)
            logf.close()
            time.sleep(2)
            isolation.safe_rmtree(env['XDG_RUNTIME_DIR'], work)
            db = profdir / 'database.sqlite'
            log = profdir / 'log.txt'
            warns = [l[:160] for l in log.read_text(errors='replace').splitlines()
                     if 'Could not upgrade' in l or 'Migration failed' in l] if log.exists() else ['NO-LOG']
            results[name] = {'alive': alive, 'db': str(db.relative_to(ROOT)),
                             'state': db_state(str(db)) if db.exists() else None,
                             'warnings': warns}
            print(name, json.dumps(results[name])[:400], flush=True)
    finally:
        server.terminate()
        server.wait(timeout=5)
        os.close(fd)
        try:
            isolation.safe_unlink(work / 'display-probe', work)
        except FileNotFoundError:
            pass
    if out:
        out.write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2)[:2000])


if __name__ == '__main__':
    main()
