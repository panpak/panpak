#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Mu SIGSEGV rate: 50 starts x 3 configs (sandbox / tree-direct / original).

Usage: python3 tests/mu_crash_rate.py [runs] [out-dir]
Rebuild release first. Uses dedicated Xvfb, isolated HOME; never the desktop.
Configs:
  sandbox  launcher via panpak-stub (Landlock on)
  direct   tree AppRun executed directly, same env (no Landlock)
  original upstream AppImage with userspace libfuse (no panpak at all)
Each run wipes the Mu profile dirs first, waits up to TIMEOUT, records
exit code/signal vs still-alive, then stops the process group (SIGTERM, SIGKILL for what is left).
Environment variables: PANPAK_PILOT_DIR (original AppImages,
default ~/panpak-bench/pilot), PANPAK_FUSE_LIB (userspace libfuse,
default ~/fuse).
"""
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
from pilot_cycle import environment  # noqa: E402
import isolation  # noqa: E402

RUNS = int(sys.argv[1]) if len(sys.argv) > 1 else 50
WORK = Path(sys.argv[2]).resolve() if len(sys.argv) > 2 else (ROOT / 'target/mu-rate')
TIMEOUT = 8.0
ORIG = Path(os.environ.get('PANPAK_PILOT_DIR', str(Path.home() / 'panpak-bench/pilot'))) / 'MuEditor-Linux-1.2.0-x86_64.AppImage'
PKG = ROOT / 'target/pilot-current/packages/mu.panpak'
FUSE = os.environ.get('PANPAK_FUSE_LIB', str(Path.home() / 'fuse2')) + '/lib/x86_64-linux-gnu'


def pkg_root(pkg):
    with pkg.open('rb') as f:
        f.seek(-80, 2)
        return struct.unpack('<8sIIQQQQ32s', f.read(80))[-1].hex()


def wipe_mu_profile(home):
    for rel in ('.local/share/mu', '.local/share/virtualenv', '.local/share/jupyter',
                '.ipython', 'mu_code', '.config/mu', '.cache/mu'):
        p = home / rel
        if p.is_symlink() or p.is_file():
            isolation.safe_unlink(p, WORK)
        elif p.is_dir():
            isolation.safe_rmtree(p, WORK)


def install(home):
    home.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'))
    p = subprocess.run([BIN / 'panpak', 'install', '--yes', '--runtime',
                        BIN / 'panpak-stub', PKG], env=env,
                       capture_output=True, text=True, timeout=180)
    assert p.returncode == 0, p.stderr


def one_run(argv, env, cwd):
    t0 = time.perf_counter()
    p = subprocess.Popen([str(a) for a in argv], env=env, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, start_new_session=True, cwd=str(cwd))
    rc = None
    while time.perf_counter() - t0 < TIMEOUT:
        rc = p.poll()
        if rc is not None:
            break
        time.sleep(0.2)
    dt = time.perf_counter() - t0
    alive = rc is None
    isolation.stop_group(p.pid, p)
    return {'alive': alive, 'returncode': rc, 'elapsed': round(dt, 2)}


def main():
    isolation.require_isolated_home(WORK / 'home', WORK)
    isolation.require_isolated_home(WORK / 'home-orig', WORK)
    WORK.mkdir(parents=True, exist_ok=True)
    (WORK / 'packages').mkdir(exist_ok=True)
    if not (WORK / 'packages/mu.panpak').exists():
        shutil.copy(PKG, WORK / 'packages/mu.panpak')
    results_path = WORK / 'results.json'
    results = json.loads(results_path.read_text()) if results_path.exists() else {}
    xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
    fd = os.open(WORK / 'display', os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
    server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x900x24'],
                              pass_fds=[fd], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            text = (WORK / 'display').read_text().strip()
            if text:
                break
            time.sleep(0.1)
        assert text, 'no Xvfb'
        display = ':' + text

        home = WORK / 'home'
        if not (home / '.local/bin/org.example.mu').exists():
            install(home)
        tree = home / '.local/share/panpak/trees' / pkg_root(PKG)
        if not (tree / 'AppRun').exists():
            # Materialization happens at first start: a short run is enough.
            env0 = environment(home, 'mu', display)
            one_run([home / '.local/bin/org.example.mu'], env0,
                    home / '.local/share/org.example.mu')
            isolation.safe_rmtree(env0['XDG_RUNTIME_DIR'], WORK)
            wipe_mu_profile(home)
        assert (tree / 'AppRun').exists(), tree

        home_orig = WORK / 'home-orig'
        home_orig.mkdir(parents=True, exist_ok=True)

        for cfg in ('sandbox', 'direct', 'original'):
            if cfg in results and len(results[cfg]) >= RUNS:
                print(f'{cfg}: already measured {len(results[cfg])}, skipping', flush=True)
                continue
            rows = results.get(cfg, [])
            for i in range(len(rows), RUNS):
                if cfg == 'original':
                    wipe_mu_profile(home_orig)
                    rt = WORK / f'rt-orig-{os.getpid()}-{i}'
                    rt.mkdir(exist_ok=True)
                    # Same Qt/X11 env as the sandbox (all else equal:
                    # without QT_QPA_PLATFORM the original aborts over config,
                    # not over the bug under test). Only HOME stays separate.
                    env = dict(os.environ, HOME=str(home_orig),
                               XDG_DATA_HOME=str(home_orig / '.local/share'),
                               XDG_CONFIG_HOME=str(home_orig / '.config'),
                               XDG_CACHE_HOME=str(home_orig / '.cache'),
                               DISPLAY=display, XDG_SESSION_TYPE='x11',
                               GDK_BACKEND='x11', QT_QPA_PLATFORM='xcb',
                               XAUTHORITY='', GSETTINGS_BACKEND='memory',
                               QT_X11_NO_MITSHM='1',
                               XDG_RUNTIME_DIR=str(rt),
                               LD_LIBRARY_PATH=FUSE + ':' + os.environ.get('LD_LIBRARY_PATH', ''))
                    rows.append(one_run([ORIG], env, home_orig))
                    isolation.safe_rmtree(rt, WORK)
                else:
                    wipe_mu_profile(home)
                    env = environment(home, 'mu', display)
                    appdata = home / '.local/share/org.example.mu'
                    if cfg == 'sandbox':
                        argv = [home / '.local/bin/org.example.mu']
                    else:
                        argv = [tree / 'AppRun']
                    rows.append(one_run(argv, env, appdata))
                    isolation.safe_rmtree(env['XDG_RUNTIME_DIR'], WORK)
                results[cfg] = rows
                results_path.write_text(json.dumps(results, indent=2))
                if (i + 1) % 10 == 0:
                    print(f'{cfg}: {i + 1}/{RUNS}', flush=True)
            print(f'{cfg} done', [r['returncode'] for r in rows][:5], '...', flush=True)
    finally:
        server.terminate()
        server.wait(timeout=5)
        os.close(fd)


if __name__ == '__main__':
    main()
