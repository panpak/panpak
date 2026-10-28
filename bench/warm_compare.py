#!/usr/bin/env python3
"""Warm/cold start: original vs Panpak cache-on vs Panpak cache-off.

Usage: python3 bench/warm_compare.py [out.json] [cli-only|joplin]
3 different apps, same finish line per side:
  AIL3 (small):  ail-cli --help, exit 0
  QON9 (Qt):       QOwnNotes --version, exit 0 (QT_QPA_PLATFORM=offscreen)
  Joplin (Electron, works under Panpak): first mapped window
Columns: original / panpak cache on / panpak PANPAK_NO_ABI_CACHE=1.
10 warm runs (2 discarded warmups) + 10 cold runs (evict before each run),
median/min/max/stdev. AppImages via userspace FUSE, no extraction.
Environment variables (see bench/README.md):
  PANPAK_BENCH_DIR (default ~/panpak-bench),
  PANPAK_PKGS_DIR (default temporary directory "panpak-bench-pkgs"),
  PANPAK_PKG_EXT (default ".panpak"),
  PANPAK_FUSE_LIB (default ~/fuse2/lib/x86_64-linux-gnu),
  PANPAK_PILOT_ORIG (profile: directory containing the original Joplin AppImage).
"""
import json
import os
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
sys.path.insert(0, str(ROOT / 'bench'))
from bench import evict, BASE_ENV  # noqa: E402
import isolation  # noqa: E402

BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
FUSE = os.environ.get('PANPAK_FUSE_LIB', str(Path.home() / 'fuse2/lib/x86_64-linux-gnu'))
BP = Path(os.environ.get('PANPAK_PKGS_DIR', os.path.join(tempfile.gettempdir(), 'panpak-bench-pkgs')))
BB = Path(os.environ.get('PANPAK_BENCH_DIR', str(Path.home() / 'panpak-bench')))
EXT = os.environ.get('PANPAK_PKG_EXT', '.panpak')
PILOT_ORIG = Path(os.environ.get('PANPAK_PILOT_ORIG', str(Path.home() / 'panpak-bench/pilot')))
WORK = ROOT / 'target/warmcmp'
REPS = 10
WARMUP = 2


def stats(name, dts):
    dts = sorted(dts)
    n = len(dts)
    med = dts[n // 2] if n % 2 else (dts[n // 2 - 1] + dts[n // 2]) / 2
    return {'name': name, 'ok': True, 'n': n, 'median': med, 'min': dts[0],
            'max': dts[-1], 'stdev': statistics.pstdev(dts) if n > 1 else 0.0}


def run_exit(argv, env, timeout=120):
    t0 = time.perf_counter()
    p = subprocess.run([str(a) for a in argv], env=env, capture_output=True,
                       text=True, timeout=timeout)
    dt = time.perf_counter() - t0
    return (dt, True) if p.returncode == 0 else (None, False)


def measure_cli(name, argv, env, files, extra_env=None):
    out = {}
    env = dict(env, **(extra_env or {}))
    for label, cold in (('hot', False), ('cold', True)):
        if not cold:
            for _ in range(WARMUP):
                run_exit(argv, env)
        dts = []
        for _ in range(REPS):
            if cold:
                evict(files)
            dt, ok = run_exit(argv, env)
            assert ok, f'{name} {label} failed'
            dts.append(dt)
        out[label] = stats(f'{name}-{label}', dts)
        print(name, label, round(out[label]['median'], 3), flush=True)
    return out


def pkg_root(pkg):
    with pkg.open('rb') as f:
        f.seek(-80, 2)
        return struct.unpack('<8sIIQQQQ32s', f.read(80))[-1].hex()


def install(home, pkg):
    env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'))
    p = subprocess.run([BIN / 'panpak', 'install', '--yes', '--runtime',
                        BIN / 'panpak-stub', pkg], env=env, capture_output=True,
                       text=True, timeout=180)
    assert p.returncode == 0, p.stderr


def main():
    only = sys.argv[2] if len(sys.argv) > 2 else 'all'
    out_path = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else WORK / 'warm-compare.json'
    isolation.require_isolated_home(WORK / 'home', WORK)
    isolation.require_isolated_home(WORK / 'home-orig', WORK)
    WORK.mkdir(parents=True, exist_ok=True)
    res = json.loads(out_path.read_text()) if out_path.exists() else {}
    home = WORK / 'home'
    home.mkdir(parents=True, exist_ok=True)
    (WORK / 'packages').mkdir(exist_ok=True)
    jp_pilot = ROOT / ('target/pilot-current/packages/joplin' + EXT)
    if not (WORK / ('packages/joplin' + EXT)).exists():
        shutil.copy(jp_pilot, WORK / ('packages/joplin' + EXT))
    stub = BIN / 'panpak-stub'

    pkgs = {'ail': (BP / ('ail-3.0.0' + EXT), BB / 'appimagelauncher-lite-3.0.0-beta-2-gha287-x86_64.AppImage',
                    'org.example.ail', ['--help'], {}),
            'qon': (BP / ('qon-26.10.9' + EXT), BB / 'QON-v26.10.9-QOwnNotes-x86_64.AppImage',
                    'org.example.qownnotes', ['--version'], {'QT_QPA_PLATFORM': 'offscreen'})}
    if only in ('all', 'cli-only'):
        for app, (pkg, ai, app_id, args, xenv) in pkgs.items():
            if app in res:
                continue
            install(home, pkg)
            tree = home / '.local/share/panpak/trees' / pkg_root(pkg)
            launcher = home / f'.local/bin/{app_id}'
            nenv = dict(BASE_ENV, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'), **xenv)
            oenv = dict(BASE_ENV, **xenv)
            files_n = [str(pkg), str(tree), str(stub)]
            cols = {'original': measure_cli(f'{app}-orig', [ai, *args], oenv, [str(ai)])}
            cols['panpak-on'] = measure_cli(f'{app}-on', [launcher, *args], nenv, files_n)
            cols['panpak-off'] = measure_cli(f'{app}-off', [launcher, *args], nenv, files_n,
                                            {'PANPAK_NO_ABI_CACHE': '1'})
            res[app] = cols
            out_path.write_text(json.dumps(res, indent=2))

    if only in ('all', 'joplin'):
        have = set(res.get('joplin', {}))
        if not {'original', 'panpak-on', 'panpak-off'} <= have:
            from pilot_cycle import environment
            jp = ROOT / ('target/pilot-current/packages/joplin' + EXT)
            install(home, jp)
            jlaunch = home / '.local/bin/org.example.joplin'
            jorig = PILOT_ORIG / 'Joplin-3.7.21.AppImage'
            xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
            fd = os.open(WORK / 'display', os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
            server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x900x24'],
                                      pass_fds=[fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                for _ in range(100):
                    text = (WORK / 'display').read_text().strip()
                    if text:
                        break
                    time.sleep(0.1)
                display = ':' + text

                def gone(timeout=15):
                    t0 = time.perf_counter()
                    while time.perf_counter() - t0 < timeout:
                        left = subprocess.run(['pgrep', '-x', 'joplin'], capture_output=True, text=True)
                        if left.stdout.strip() == '':
                            return True
                        time.sleep(0.5)
                    return False

                def window_time(argv, env, cwd):
                    assert gone(), 'leftover Joplin processes before run'
                    fails = 0
                    while True:
                        t0 = time.perf_counter()
                        p = subprocess.Popen([str(a) for a in argv], env=env,
                                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                             start_new_session=True, cwd=str(cwd))
                        dt = None
                        try:
                            while time.perf_counter() - t0 < 90:
                                w = subprocess.run(['xwininfo', '-root', '-tree'],
                                                   env={'DISPLAY': display}, capture_output=True,
                                                   text=True).stdout
                                if '"Joplin"' in w or 'appimagekit-joplin' in w:
                                    dt = time.perf_counter() - t0
                                    break
                                if p.poll() is not None:
                                    break
                                time.sleep(0.2)
                        finally:
                            isolation.stop_group(p.pid, p)
                            assert gone(), 'Joplin does not exit after stop'
                        if dt is not None:
                            return dt, fails
                        fails += 1
                        w = subprocess.run(['xwininfo', '-root', '-tree'],
                                           env={'DISPLAY': display}, capture_output=True,
                                           text=True).stdout
                        (WORK / 'joplin-nowindow.txt').write_text(
                            f'fails={fails}\nargv={argv[0]}\n' + w)
                        assert fails < 2, 'Joplin window never appeared (2 attempts)'

                # Warm profiles (migrations already done): one per side.
                prof_n = home / '.local/share/org.example.joplin/warm-joplin'
                prof_o = WORK / 'warm-orig'
                for d in (prof_n, prof_o):
                    d.mkdir(parents=True, exist_ok=True)
                env_n = environment(home, 'joplin', display)
                rt = Path(isolation.owned_mkdtemp('ngrt-warm-'))
                env_o = dict(BASE_ENV, HOME=str(WORK / 'home-orig'), DISPLAY=display,
                             XDG_SESSION_TYPE='x11', XDG_RUNTIME_DIR=str(rt),
                             LD_LIBRARY_PATH=FUSE + ':' + BASE_ENV.get('LD_LIBRARY_PATH', ''))
                Path(WORK / 'home-orig').mkdir(exist_ok=True)
                env_o.pop('WAYLAND_DISPLAY', None)
                for argv, env, cwd in (
                        ([jlaunch, '--no-sandbox', '--disable-gpu'], env_n,
                         home / '.local/share/org.example.joplin'),
                        ([jorig, '--no-sandbox', '--disable-gpu',
                          f'--user-data-dir={prof_o}'], env_o, WORK)):
                    p = subprocess.Popen([str(a) for a in argv], env={k: str(v) for k, v in env.items()},
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                         start_new_session=True, cwd=str(cwd))
                    time.sleep(45)
                    isolation.stop_group(p.pid, p)
                isolation.safe_rmtree(env_n['XDG_RUNTIME_DIR'], WORK)

                tree = home / '.local/share/panpak/trees' / pkg_root(jp)
                cols = dict(res.get('joplin', {}))
                cases = [('original', [jorig, '--no-sandbox', '--disable-gpu',
                                       f'--user-data-dir={prof_o}'], env_o, WORK, [str(jorig)]),
                         ('panpak-on', [jlaunch, '--no-sandbox', '--disable-gpu'], env_n,
                          home / '.local/share/org.example.joplin', [str(jp), str(tree), str(stub)]),
                         ('panpak-off', [jlaunch, '--no-sandbox', '--disable-gpu'], dict(env_n, PANPAK_NO_ABI_CACHE='1'),
                          home / '.local/share/org.example.joplin', [str(jp), str(tree), str(stub)])]
                for cname, argv, env, cwd, files in cases:
                    if cname in cols:
                        print('joplin', cname, 'already measured, skipping', flush=True)
                        continue
                    out = {}
                    for label, cold in (('hot', False), ('cold', True)):
                        if not cold:
                            for _ in range(WARMUP):
                                window_time(argv, env, cwd)
                        dts = []
                        retries = 0
                        for _ in range(REPS):
                            if cold:
                                evict(files)
                            dt, fails = window_time(argv, env, cwd)
                            dts.append(dt)
                            retries += fails
                        out[label] = stats(f'joplin-{cname}-{label}', dts)
                        out[label]['retries'] = retries
                        print('joplin', cname, label, round(out[label]['median'], 3),
                              f'(retry {retries})', flush=True)
                        left = subprocess.run(['pgrep', '-x', 'joplin'], capture_output=True, text=True)
                        assert left.stdout.strip() == '', f'leftovers: {left.stdout}'
                    cols[cname] = out
                    res['joplin'] = cols
                    out_path.write_text(json.dumps(res, indent=2))
                isolation.safe_rmtree(rt, WORK)
            finally:
                server.terminate()
                server.wait(timeout=5)
                os.close(fd)
    print('written', out_path)


if __name__ == '__main__':
    main()
