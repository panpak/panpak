#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Public benchmarks: update size, first and later starts, cost of the view, resolution.

Usage: python3 bench/public_bench.py [delta|start|view|resolution|all] [--out DIR]

Build the release binaries first (cargo build --release --target x86_64-unknown-linux-musl).
The inputs are official AppImages with a pinned SHA-256 (bench/inputs.py); missing ones are
downloaded into $PANPAK_BENCH_DIR/public. Nothing runs outside an isolated HOME below the
work directory ($PANPAK_PUBLIC_BENCH_WORK, default target/public-bench).

Each measurement writes one CSV file in docs/bench-data/ (or --out). The lines that start
with `#` at the top hold the machine, the system, the versions and the date.

Methods (details in docs/BENCHMARKS.md):
  delta       `panpak update` against a local HTTP server that counts the bytes it sends.
              Two consecutive versions of three apps; no number is estimated.
  start       time of an exiting command: first start after install (the tree is built),
              later starts (warm and cold), and the original AppImage (userspace FUSE).
  view        later starts with the isolated view and with bubblewrap made unusable.
  resolution  later starts with the resolution cache on and off (PANPAK_NO_ABI_CACHE=1).
"""
import csv
import datetime
import json
import os
from pathlib import Path
import platform
import shutil
import socket
import statistics
import struct
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tests'))
sys.path.insert(0, str(ROOT / 'bench'))
import inputs  # noqa: E402
import isolation  # noqa: E402

BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
WORK = Path(os.environ.get('PANPAK_PUBLIC_BENCH_WORK', str(ROOT / 'target/public-bench'))).resolve()
FUSE = os.environ.get('PANPAK_FUSE_LIB', str(Path.home() / 'fuse2/lib/x86_64-linux-gnu'))
REPS = 10
WARMUP = 2
FIRST_REPS = 5

# Exiting commands, the same on every side. `old` and `new` are consecutive versions.
APPS = {
    'ail': dict(old='ail-2.2.0', new='ail-3.0.0-beta-3', id='org.example.ail', convert=['--entry', 'usr/bin/ail-cli'],
                args=['--help'], env={}, what='ail-cli --help', expect='Usage'),
    'qownnotes': dict(old='qon-26.10.8', new='qon-26.10.9', id='org.example.qownnotes',
                      convert=['--allow-missing', 'libQt6Quick*', '--allow-missing', 'libQt6Qml*', '--allow-missing', 'libQt6Sql*',
                               '--allow-missing', 'libQt6WebSockets*', '--allow-missing', 'libQt6Xml*', '--allow-missing', 'libicu*',
                               '--allow-missing', 'libpcre.so.3', '--allow-missing', 'libunistring.so.2'],
                      args=['--version'], env={'QT_QPA_PLATFORM': 'offscreen'}, what='--version, QT_QPA_PLATFORM=offscreen',
                      expect='QOwnNotes 26.10.9'),
    # Simplenote is only used for the update size: it is an Electron app with no command that exits.
    'simplenote': dict(old='sn-2.27.0', new='sn-2.27.1', id='org.example.simplenote', convert=[], args=None, env={}, what=None),
    # PrusaSlicer has no older version in the inputs: used for the starts, not for the update size.
    'prusaslicer': dict(old=None, new='prusaslicer-2.8.1', id='org.example.prusaslicer', convert=[], args=['--help'], env={},
                        what='--help', expect='PrusaSlicer'),
}
DELTA_APPS = [n for n, a in APPS.items() if a['old']]
START_APPS = [n for n, a in APPS.items() if a['args']]
MASK = ['/usr/bin/bwrap', '--unshare-user', '--bind', '/', '/', '--dev-bind', '/dev', '/dev', '--proc', '/proc',
        '--ro-bind', '/dev/null', '/usr/bin/bwrap', '--']


# ---------------------------------------------------------------- helpers

def rmtree(p):
    if Path(p).exists():
        subprocess.run(['chmod', '-R', 'u+w', str(p)], check=False)
        shutil.rmtree(p)


def stats(dts):
    dts = sorted(dts)
    n = len(dts)
    med = dts[n // 2] if n % 2 else (dts[n // 2 - 1] + dts[n // 2]) / 2
    return dict(n=n, median_s=med, min_s=dts[0], max_s=dts[-1], stdev_s=statistics.pstdev(dts) if n > 1 else 0.0)


LAST = None


def timed(argv, env, timeout=300):
    """Seconds a command takes; it must exit 0. The last result is kept in LAST."""
    global LAST
    t0 = time.perf_counter_ns()
    p = subprocess.run([str(a) for a in argv], env=env, capture_output=True, text=True, timeout=timeout,
                       stdin=subprocess.DEVNULL)
    dt = (time.perf_counter_ns() - t0) / 1e9
    LAST = p
    if p.returncode != 0:
        raise SystemExit(f'command failed ({p.returncode}): {argv}\n{p.stdout[-400:]}{p.stderr[-800:]}')
    return dt


def evict(paths):
    """Drop the files from the page cache (POSIX_FADV_DONTNEED), without privileges."""
    for p in paths:
        p = str(p)
        if os.path.isdir(p):
            for root, _, files in os.walk(p):
                evict([os.path.join(root, f) for f in files])
        elif os.path.isfile(p):
            try:
                fd = os.open(p, os.O_RDONLY)
                try:
                    os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
                finally:
                    os.close(fd)
            except OSError:
                pass


def sh(*argv, **kw):
    return subprocess.run(argv, capture_output=True, text=True, **kw).stdout.strip()


def metadata():
    cpu = next((l.split(':', 1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')), '?')
    mem = next((int(l.split()[1]) for l in open('/proc/meminfo') if l.startswith('MemTotal')), 0)
    osr = dict(l.rstrip().split('=', 1) for l in open('/etc/os-release') if '=' in l)
    dirty = bool(sh('git', '-C', str(ROOT), 'status', '--porcelain', '--untracked-files=no'))
    doctor = sh(str(BIN / 'panpak'), 'doctor')
    landlock = next((l for l in doctor.splitlines() if 'Landlock' in l), '').strip()
    bwrap = sh('/usr/bin/bwrap', '--version')
    return [
        ('date_utc', datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')),
        ('cpu', cpu), ('threads', os.cpu_count()), ('ram_mib', mem // 1024),
        ('kernel', platform.release()), ('os', osr.get('PRETTY_NAME', '?').strip('"')),
        ('filesystem_of_work_dir', sh('df', '--output=fstype', str(WORK)).splitlines()[-1].strip()),
        ('landlock', landlock), ('bubblewrap', bwrap), ('python', platform.python_version()),
        ('rustc', sh('rustc', '--version')),
        ('panpak_commit', sh('git', '-C', str(ROOT), 'rev-parse', '--short', 'HEAD') + ('-dirty' if dirty else '')),
        ('panpak_sha256', sh('sha256sum', str(BIN / 'panpak')).split()[0][:16]),
        ('stub_sha256', sh('sha256sum', str(BIN / 'panpak-stub')).split()[0][:16]),
    ]


def write_csv(out_dir, name, meta, header, rows):
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / name
    with open(path, 'w', newline='') as f:
        for k, v in meta:
            f.write(f'# {k}: {v}\n')
        w = csv.writer(f, lineterminator='\n')
        w.writerow(header)
        for r in rows:
            w.writerow([f'{r[h]:.4f}' if isinstance(r[h], float) else r[h] for h in header])
    print('wrote', path, flush=True)


class Lab:
    """One isolated HOME and runtime folder under the work directory."""

    def __init__(self, tag):
        self.home = WORK / 'homes' / tag
        rmtree(self.home)
        self.home.mkdir(parents=True)
        isolation.require_isolated_home(self.home, WORK)
        self.runtime = isolation.owned_mkdtemp('pb-rt-')
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.home), 'XDG_DATA_HOME': str(self.home / '.local/share'),
                    'XDG_RUNTIME_DIR': self.runtime, 'LANG': os.environ.get('LANG', 'C.UTF-8')}

    def panpak(self, *args, env=None, timeout=300):
        return timed([BIN / 'panpak', *args], dict(self.env, **(env or {})), timeout)

    def install(self, pkg):
        return self.panpak('install', '--yes', '--accept-new-permissions', '--runtime', BIN / 'panpak-stub', pkg)

    def launcher(self, app_id):
        return self.home / '.local/bin' / app_id

    def close(self):
        shutil.rmtree(self.runtime, ignore_errors=True)
        rmtree(self.home)


def pkg_root(pkg):
    with open(pkg, 'rb') as f:
        f.seek(-80, 2)
        return struct.unpack('<8sIIQQQQ32s', f.read(80))[-1].hex()


def convert_all(keys):
    """Convert the AppImages to packages (cached in the work directory)."""
    pk = WORK / 'packages'
    pk.mkdir(parents=True, exist_ok=True)
    key = WORK / 'secret.key'
    if not key.exists():
        subprocess.run([BIN / 'panpak', 'keygen', '-o', key], check=True, capture_output=True)
    out = {}
    env = {'PATH': os.environ['PATH'], 'HOME': str(WORK / 'homes/convert')}
    (WORK / 'homes/convert').mkdir(parents=True, exist_ok=True)
    for name, app in APPS.items():
        for which in ('old', 'new'):
            k = app[which]
            if k is None or k not in keys:
                continue
            dest = pk / f'{k}.panpak'
            if not dest.exists():
                item = inputs.BY_KEY[k]
                p = subprocess.run([BIN / 'panpak', 'convert', inputs.path_of(k), '--id', app['id'], '--stub', BIN / 'panpak-stub',
                                    '--secret-file', key, '--name', name, '--version', item['version'], *app['convert'],
                                    '-o', dest], env=env, capture_output=True, text=True)
                if p.returncode != 0:
                    raise SystemExit(f'convert {k} failed:\n{p.stdout}{p.stderr}')
            out[k] = dest
    return out


# ---------------------------------------------------------------- measurements

def measure_delta(out_dir, meta):
    import incremental
    keys = [APPS[n][w] for n in DELTA_APPS for w in ('old', 'new')]
    inputs.fetch(keys)
    pkgs = convert_all(keys)
    rows = []
    for name in DELTA_APPS:
        app = APPS[name]
        lab = Lab(f'delta-{name}')
        work = WORK / f'delta-{name}'
        rmtree(work)
        work.mkdir(parents=True)
        s = socket.socket()
        s.bind(('127.0.0.1', 0))
        port = s.getsockname()[1]
        s.close()
        old = incremental.resign(pkgs[app['old']].read_bytes(), '1.0', port)
        new = incremental.resign(pkgs[app['new']].read_bytes(), '1.1', port)
        (work / 'old.panpak').write_bytes(old)
        (work / 'remote.panpak').write_bytes(new)
        _, chunks = incremental.index(new)
        _, old_chunks = incremental.index(old)
        have = {(h, comp, raw) for h, _, comp, raw in old_chunks}
        missing = [c for c in chunks if (c[0], c[2], c[3]) not in have]
        lab.install(work / 'old.panpak')
        log = work / 'ranges.jsonl'
        server = subprocess.Popen([sys.executable, ROOT / 'tests/range_server.py', work, str(port), 'normal'],
                                  env=dict(os.environ, PANPAK_RANGE_LOG=str(log)),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(0.5)
            t = lab.panpak('update', '--allow-insecure-http', '--timeout', '60', 'org.example.incremental')
            client = int(next(l for l in LAST.stdout.splitlines() if l.startswith('downloaded bytes:')).split(':')[1])
        finally:
            server.terminate()
            server.wait(timeout=5)
        reqs = [json.loads(l) for l in log.read_text().splitlines()]
        got = sum(r['bytes'] for r in reqs)
        current = lab.home / '.local/share/panpak/apps/org.example.incremental/current'
        assert current.resolve().read_bytes() == new, 'the updated package differs from the remote one'
        rows.append(dict(app=name, old_version=inputs.BY_KEY[app['old']]['version'], new_version=inputs.BY_KEY[app['new']]['version'],
                         appimage_new_bytes=inputs.BY_KEY[app['new']]['size'], panpak_new_bytes=len(new), downloaded_bytes=got,
                         client_counted_bytes=client,
                         downloaded_percent=100 * got / len(new), requests=len(reqs), new_chunks=len(missing),
                         total_chunks=len(chunks), update_seconds=t))
        lab.close()
        rmtree(work)
        print(name, 'downloaded', got, 'of', len(new), flush=True)
    write_csv(out_dir, 'delta.csv', meta,
              ['app', 'old_version', 'new_version', 'appimage_new_bytes', 'panpak_new_bytes', 'downloaded_bytes',
               'client_counted_bytes', 'downloaded_percent', 'requests', 'new_chunks', 'total_chunks', 'update_seconds'], rows)


def app_setup(name):
    app = APPS[name]
    inputs.fetch([app['new']])
    return app, convert_all([app['new']])[app['new']]


def series(argv, env, n, warmup, files=None):
    for _ in range(warmup):
        timed(argv, env)
    dts = []
    for _ in range(n):
        if files:
            evict(files)
        dts.append(timed(argv, env))
    return stats(dts)


def measure_start(out_dir, meta):
    rows = []
    for name in START_APPS:
        app, pkg = app_setup(name)
        ai = inputs.path_of(app['new'])
        version = inputs.BY_KEY[app['new']]['version']
        installs, firsts = [], []
        lab = None
        for i in range(FIRST_REPS):
            if lab:
                lab.close()
            lab = Lab(f'start-{name}')
            installs.append(lab.install(pkg))
            firsts.append(timed([lab.launcher(app['id']), *app['args']], dict(lab.env, **app['env'])))
            assert app['expect'].lower() in (LAST.stdout + LAST.stderr).lower(), f'{name}: unexpected output {LAST.stdout[:200]}'
        env = dict(lab.env, **app['env'])
        launcher = lab.launcher(app['id'])
        tree = lab.home / '.local/share/panpak/trees' / pkg_root(pkg)
        files = [pkg, tree, BIN / 'panpak-stub']
        base = dict(app=name, version=version, command=app['what'])
        rows.append(dict(base, variant='panpak-install', **stats(installs)))
        rows.append(dict(base, variant='panpak-first-start', **stats(firsts)))
        rows.append(dict(base, variant='panpak-later-warm', **series([launcher, *app['args']], env, REPS, WARMUP)))
        rows.append(dict(base, variant='panpak-later-cold', **series([launcher, *app['args']], env, REPS, 0, files)))
        aenv = dict(env, LD_LIBRARY_PATH=FUSE)
        rows.append(dict(base, variant='appimage-warm', **series([ai, *app['args']], aenv, REPS, WARMUP)))
        rows.append(dict(base, variant='appimage-cold', **series([ai, *app['args']], aenv, REPS, 0, [ai])))
        lab.close()
        print(name, {r['variant']: round(r['median_s'], 3) for r in rows if r['app'] == name}, flush=True)
    write_csv(out_dir, 'start.csv', meta, ['app', 'version', 'command', 'variant', 'n', 'median_s', 'min_s', 'max_s', 'stdev_s'], rows)


def measure_view(out_dir, meta):
    rows = []
    for name in START_APPS:
        app, pkg = app_setup(name)
        lab = Lab(f'view-{name}')
        lab.install(pkg)
        env = dict(lab.env, **app['env'])
        launcher = lab.launcher(app['id'])
        base = dict(app=name, version=inputs.BY_KEY[app['new']]['version'], command=app['what'])
        timed([launcher, *app['args']], env)  # first start: the tree
        # Alternate the two sides in short blocks so that slow drifts of the machine hit both.
        view, plain = [], []
        for _ in range(WARMUP):
            timed([launcher, *app['args']], env)
            timed([*MASK, launcher, *app['args']], env)
        for _ in range(REPS):
            view.append(timed([launcher, *app['args']], env))
            plain.append(timed([*MASK, launcher, *app['args']], env))
        rows.append(dict(base, variant='view', **stats(view)))
        rows.append(dict(base, variant='no-view', **stats(plain)))
        lab.close()
        print(name, 'view', round(stats(view)['median_s'], 3), 'no-view', round(stats(plain)['median_s'], 3), flush=True)
    # Control: what the wrapper that disables bubblewrap costs on its own.
    ctl_env = {'PATH': os.environ['PATH']}
    a, b = [], []
    for _ in range(WARMUP + 30):
        a.append(timed(['/bin/true'], ctl_env))
        b.append(timed([*MASK, '/bin/true'], ctl_env))
    base = dict(app='(control)', version='', command='/bin/true')
    rows.append(dict(base, variant='plain-true', **stats(a[WARMUP:])))
    rows.append(dict(base, variant='wrapper-true', **stats(b[WARMUP:])))
    write_csv(out_dir, 'view.csv', meta, ['app', 'version', 'command', 'variant', 'n', 'median_s', 'min_s', 'max_s', 'stdev_s'], rows)


def measure_resolution(out_dir, meta):
    rows = []
    for name in START_APPS:
        app, pkg = app_setup(name)
        lab = Lab(f'res-{name}')
        lab.install(pkg)
        launcher = lab.launcher(app['id'])
        base = dict(app=name, version=inputs.BY_KEY[app['new']]['version'], command=app['what'])
        env_on = dict(lab.env, **app['env'])
        env_off = dict(env_on, PANPAK_NO_ABI_CACHE='1')
        timed([launcher, *app['args']], env_on)
        on, off = [], []
        for _ in range(WARMUP):
            timed([launcher, *app['args']], env_on)
            timed([launcher, *app['args']], env_off)
        for _ in range(REPS):
            on.append(timed([launcher, *app['args']], env_on))
            off.append(timed([launcher, *app['args']], env_off))
        rows.append(dict(base, variant='resolution-cache-on', **stats(on)))
        rows.append(dict(base, variant='resolution-cache-off', **stats(off)))
        lab.close()
        print(name, 'on', round(stats(on)['median_s'], 3), 'off', round(stats(off)['median_s'], 3), flush=True)
    write_csv(out_dir, 'resolution.csv', meta, ['app', 'version', 'command', 'variant', 'n', 'median_s', 'min_s', 'max_s', 'stdev_s'], rows)


def main():
    args = sys.argv[1:]
    out_dir = ROOT / 'docs/bench-data'
    if '--out' in args:
        i = args.index('--out')
        out_dir = Path(args[i + 1]).resolve()
        del args[i:i + 2]
    what = args[0] if args else 'all'
    WORK.mkdir(parents=True, exist_ok=True)
    meta = metadata()
    todo = {'delta': measure_delta, 'start': measure_start, 'view': measure_view, 'resolution': measure_resolution}
    for k, fn in todo.items():
        if what in ('all', k):
            fn(out_dir, meta)


if __name__ == '__main__':
    main()
