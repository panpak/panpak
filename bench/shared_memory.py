#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Memory of apps that run at the same time: what the shared files of the store save, and what the sandbox costs.

Usage: python3 bench/shared_memory.py [--apps qownnotes,simplenote] [--reps 3] [--out DIR]

Build the release binaries first. The inputs are the official AppImages of bench/inputs.py (two consecutive versions of each app). Everything runs
under a headless Sway, in an isolated HOME below the work directory ($PANPAK_SHARED_MEMORY_WORK, default target/shared-memory). Nothing touches your home.

Three arms, the same apps and the same flags in each:
  panpak    both versions installed (two app ids) in ONE store: files that are the same in the two versions are the same inode (a hard link of one blob),
            so the kernel keeps one copy of their pages. The isolated view (bubblewrap, the proxies) is part of what is measured.
  extracted each AppImage unpacked into its own folder (what an installer or a container image of each app gives): two copies of every file.
  appimage  the AppImage itself, mounted by userspace FUSE (libfuse2): two mounts, two copies of the pages.

Three cases: the new version alone, the old version alone, and both at the same time. Each app runs in its own cgroup scope (systemd-run --user --scope),
so the processes of an app are exactly the members of the scope. After the windows are up and the apps have settled, for every process of the scopes:
  PSS (smaps_rollup): the pages of the process, each page divided by the number of processes that map it. Summed over all processes of both apps, a page
  that two apps share counts once, a page that each has its own copy of counts twice. `pss_file` is the part that is a mapped file.
  A process in a user namespace of its own cannot be read by smaps_rollup: it is counted in `unreadable`, with its VmRSS (an upper bound for its PSS) in
  `unreadable_rss_kib`, and is not in the PSS sums.
  The cgroup counters (memory.current, memory.stat anon and file): what the kernel has charged to the two scopes.
  MemAvailable of the system before and after (noisy: the rest of the machine moves too).
Before every run the files of the arm are dropped from the page cache (POSIX_FADV_DONTNEED), so each run starts without them.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
os.environ.setdefault('PANPAK_PUBLIC_BENCH_WORK', os.environ.get('PANPAK_SHARED_MEMORY_WORK', str(ROOT / 'target/shared-memory')))
sys.path.insert(0, str(ROOT / 'tests'))
sys.path.insert(0, str(ROOT / 'bench'))
import inputs  # noqa: E402
import isolation  # noqa: E402
import public_bench as pb  # noqa: E402

BIN = pb.BIN
WORK = pb.WORK
UID = os.getuid()
CGROOT = Path(f'/sys/fs/cgroup/user.slice/user-{UID}.slice/user@{UID}.service/app.slice')
FIELDS = ('Rss', 'Pss', 'Pss_Anon', 'Pss_File', 'Pss_Shmem', 'Private_Dirty', 'Private_Clean')

# Flags are the same in every arm. Electron needs --no-sandbox inside the view (its own sandbox needs user namespaces, which the filter refuses).
APPS = {
    'qownnotes': dict(old='qon-26.10.8', new='qon-26.10.9', ids=('org.example.qon8', 'org.example.qon9'), env={'QT_QPA_PLATFORM': 'wayland'}, args=[],
                      convert=pb.APPS['qownnotes']['convert']),
    'simplenote': dict(old='sn-2.27.0', new='sn-2.27.1', ids=('org.example.sn270', 'org.example.sn271'), env={},
                       args=['--no-sandbox', '--disable-gpu', '--ozone-platform=wayland'], convert=[]),
}


def meminfo(key):
    for line in open('/proc/meminfo'):
        if line.startswith(key + ':'):
            return int(line.split()[1])
    raise KeyError(key)


def read_pids(unit):
    try:
        return [int(x) for x in (CGROOT / f'{unit}.scope' / 'cgroup.procs').read_text().split()]
    except OSError:
        return []


def smaps(pid):
    d = dict.fromkeys(FIELDS, 0)
    try:
        for line in open(f'/proc/{pid}/smaps_rollup'):
            p = line.split()
            if p and p[0][:-1] in d:
                d[p[0][:-1]] = int(p[1])
    except OSError:
        return None
    return d


def status_rss(pid):
    """VmRSS from /proc/<pid>/status (readable when smaps_rollup is not), in KiB; 0 when it cannot be read."""
    try:
        for line in open(f'/proc/{pid}/status'):
            if line.startswith('VmRSS:'):
                return int(line.split()[1])
    except OSError:
        pass
    return 0


def cg_stat(unit):
    out = {'current': 0, 'anon': 0, 'file': 0, 'file_mapped': 0, 'shmem': 0}
    base = CGROOT / f'{unit}.scope'
    try:
        out['current'] = int((base / 'memory.current').read_text()) // 1024
        for line in (base / 'memory.stat').read_text().splitlines():
            k, v = line.split()
            if k in out:
                out[k] = int(v) // 1024
    except OSError:
        pass
    return out


def stop_unit(unit):
    """Stop the processes of our scope: SIGTERM to each pid in it, then SIGKILL to what is left after a grace time."""
    for sig, wait in ((signal.SIGTERM, 15.0), (signal.SIGKILL, 10.0)):
        pids = read_pids(unit)
        if not pids:
            return
        for pid in pids:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
        end = time.monotonic() + wait
        while time.monotonic() < end and read_pids(unit):
            time.sleep(0.2)


class Sway:
    def __init__(self, work):
        self.runtime = Path(isolation.owned_mkdtemp('sm-rt-'))
        env = {'PATH': os.environ['PATH'], 'HOME': str(work), 'XDG_RUNTIME_DIR': str(self.runtime), 'WLR_BACKENDS': 'headless', 'WLR_LIBINPUT_NO_DEVICES': '1',
               'WLR_RENDERER': 'pixman'}
        conf = work / 'sway.conf'
        conf.write_text('output HEADLESS-1 resolution 1280x800\n')
        self.proc = subprocess.Popen(['sway', '-c', str(conf)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        self.display = None
        for _ in range(100):
            socks = [p for p in self.runtime.glob('wayland-*') if not p.name.endswith('.lock')]
            if socks:
                self.display = socks[0].name
                break
            time.sleep(0.1)
        if not self.display:
            raise SystemExit('sway did not start')
        time.sleep(0.5)
        self.ipc = sorted(self.runtime.glob('sway-ipc.*'))[0]

    def windows(self):
        p = subprocess.run(['swaymsg', '-s', str(self.ipc), '-t', 'get_tree', '-r'], capture_output=True, text=True, env={'PATH': os.environ['PATH']})
        if p.returncode:
            return 0

        def count(n):
            kids = n.get('nodes', []) + n.get('floating_nodes', [])
            if not kids and n.get('type') in ('con', 'floating_con') and n.get('pid'):
                return 1
            return sum(count(k) for k in kids)

        return count(json.loads(p.stdout))

    def close(self):
        isolation.stop_group(self.proc.pid, self.proc, grace=5)
        isolation.remove_owned(str(self.runtime), WORK)


def run_case(sway, arm, rep, label, apps, evict_paths):
    """`apps`: list of (name, argv, env). Returns one result row."""
    pb.evict(evict_paths)
    time.sleep(1.0)
    avail0 = meminfo('MemAvailable')
    units, procs = [], []
    for name, argv, env in apps:
        unit = f'pm-{os.getpid()}-{arm}-{name}-{rep}'
        cmd = ['systemd-run', '--user', '--scope', '--quiet', '--collect', '--unit', unit, '-p', 'MemoryMax=4G', '-p', 'MemorySwapMax=2G', '--', 'env', '-i',
               *[f'{k}={v}' for k, v in env.items()], *map(str, argv)]
        procs.append(subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True))
        units.append(unit)
    ok = False
    try:
        end = time.monotonic() + 120
        while time.monotonic() < end:
            if sway.windows() >= len(apps):
                ok = True
                break
            if any(p.poll() is not None for p in procs):
                break
            time.sleep(1.0)
        time.sleep(10.0)   # settle: the apps go on reading and allocating after the first window
        pids = [p for u in units for p in read_pids(u)]
        rows = [smaps(p) for p in pids]
        unreadable = sum(1 for r in rows if r is None)
        unreadable_rss = sum(status_rss(p) for p, r in zip(pids, rows) if r is None)
        rows = [r for r in rows if r]
        tot = {k: sum(r[k] for r in rows) for k in FIELDS}
        cg = [cg_stat(u) for u in units]
        avail1 = meminfo('MemAvailable')
        alive = ok and all(read_pids(u) for u in units)
    finally:
        for u in units:
            stop_unit(u)
        for p in procs:
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
    time.sleep(1.0)
    return dict(case=label, arm=arm, rep=rep, ok=int(alive), processes=len(rows), unreadable=unreadable, unreadable_rss_kib=unreadable_rss, **{f'{k.lower()}_kib': tot[k] for k in FIELDS},
                cg_current_kib=sum(c['current'] for c in cg), cg_anon_kib=sum(c['anon'] for c in cg), cg_file_kib=sum(c['file'] for c in cg),
                cg_file_mapped_kib=sum(c['file_mapped'] for c in cg), cg_shmem_kib=sum(c['shmem'] for c in cg), memavailable_drop_kib=avail0 - avail1)


def convert(name, app):
    """Packages with two ids (so that both versions can be installed in one store)."""
    pk = WORK / 'packages'
    pk.mkdir(parents=True, exist_ok=True)
    key = WORK / 'secret.key'
    if not key.exists():
        subprocess.run([BIN / 'panpak', 'keygen', '-o', key], check=True, capture_output=True)
    env = {'PATH': os.environ['PATH'], 'HOME': str(WORK / 'homes/convert')}
    (WORK / 'homes/convert').mkdir(parents=True, exist_ok=True)
    out = {}
    for which, app_id in zip(('old', 'new'), app['ids']):
        k = app[which]
        dest = pk / f'{app_id}.panpak'
        if not dest.exists():
            p = subprocess.run([BIN / 'panpak', 'convert', inputs.path_of(k), '--id', app_id, '--stub', BIN / 'panpak-stub', '--secret-file', key, '--name', name,
                                '--version', inputs.BY_KEY[k]['version'], *app['convert'], '-o', dest], env=env, capture_output=True, text=True)
            if p.returncode:
                raise SystemExit(f'convert {k} failed:\n{p.stdout}{p.stderr}')
        out[which] = dest
    return out


def extract(app):
    out = {}
    for which in ('old', 'new'):
        d = WORK / 'extracted' / app[which]
        if not (d / 'squashfs-root').is_dir():
            d.mkdir(parents=True, exist_ok=True)
            subprocess.run([str(inputs.path_of(app[which])), '--appimage-extract'], cwd=d, check=True, capture_output=True, env={'PATH': os.environ['PATH'], 'LD_LIBRARY_PATH': pb.FUSE})
        out[which] = d / 'squashfs-root'
    return out


def base_env(home, sway, extra):
    home.mkdir(parents=True, exist_ok=True)
    return dict({'PATH': os.environ['PATH'], 'HOME': str(home), 'XDG_DATA_HOME': str(home / '.local/share'), 'XDG_CONFIG_HOME': str(home / '.config'),
                 'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_RUNTIME_DIR': str(sway.runtime), 'WAYLAND_DISPLAY': sway.display, 'XDG_SESSION_TYPE': 'wayland',
                 'LANG': 'C.UTF-8', 'PANPAK_NO_BACKGROUND': '1'}, **extra)


def measure(name, app, sway, reps):
    pkgs = convert(name, app)
    ext = extract(app)
    home_p = WORK / 'homes' / f'{name}-panpak'
    isolation.require_isolated_home(home_p, WORK)
    env_p = base_env(home_p, sway, app['env'])
    for which, app_id in zip(('old', 'new'), app['ids']):
        p = subprocess.run([str(BIN / 'panpak'), 'install', '--yes', '--trust-first-key', '--accept-new-permissions', '--runtime', str(BIN / 'panpak-stub'), str(pkgs[which])], env=env_p,
                           capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=600)
        if p.returncode:
            raise SystemExit(f'install {which}: {p.stdout}{p.stderr}')
    launcher = {w: home_p / '.local/bin' / i for w, i in zip(('old', 'new'), app['ids'])}
    store = home_p / '.local/share/panpak'
    ai = {w: inputs.path_of(app[w]) for w in ('old', 'new')}

    def spec(arm, which):
        if arm == 'panpak':
            return (f'{which}', [launcher[which], *app['args']], env_p)
        if arm == 'extracted':
            return (f'{which}', [ext[which] / 'AppRun', *app['args']], base_env(WORK / 'homes' / f'{name}-ext-{which}', sway, app['env']))
        return (f'{which}', [ai[which], *app['args']], dict(base_env(WORK / 'homes' / f'{name}-ai-{which}', sway, app['env']), LD_LIBRARY_PATH=pb.FUSE))

    evict = {'panpak': [store, BIN / 'panpak-stub'], 'extracted': [ext['old'], ext['new']], 'appimage': [ai['old'], ai['new']]}
    # One start of each app first: the tree and the resolution of the store are made (that is not what is measured).
    for which in ('old', 'new'):
        run_case(sway, 'panpak', 0, 'warmup', [spec('panpak', which)], evict['panpak'])
    rows = []
    for rep in range(1, reps + 1):
        for arm in ('panpak', 'extracted', 'appimage'):
            for label, which in (('new alone', ['new']), ('old alone', ['old']), ('both', ['old', 'new'])):
                r = run_case(sway, arm, rep, label, [spec(arm, w) for w in which], evict[arm])
                r['app'] = name
                rows.append(r)
                print(name, arm, label, rep, {k: r[k] for k in ('ok', 'processes', 'pss_kib', 'pss_file_kib', 'cg_current_kib')}, flush=True)
    return rows


def summary(rows):
    out = []
    for app in sorted({r['app'] for r in rows}):
        for arm in ('panpak', 'extracted', 'appimage'):
            med = {}
            for label in ('new alone', 'old alone', 'both'):
                sel = [r for r in rows if r['app'] == app and r['arm'] == arm and r['case'] == label and r['ok']]
                med[label] = {k: statistics.median(r[k] for r in sel) for k in ('pss_kib', 'pss_file_kib', 'pss_anon_kib', 'cg_current_kib')} if sel else None
            if all(med.values()):
                alone = med['new alone']['pss_kib'] + med['old alone']['pss_kib']
                out.append(dict(app=app, arm=arm, new_alone_mib=med['new alone']['pss_kib'] / 1024, old_alone_mib=med['old alone']['pss_kib'] / 1024,
                                both_mib=med['both']['pss_kib'] / 1024, saved_mib=(alone - med['both']['pss_kib']) / 1024,
                                saved_percent=100 * (alone - med['both']['pss_kib']) / alone, both_cgroup_mib=med['both']['cg_current_kib'] / 1024))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--apps', default='qownnotes')
    ap.add_argument('--reps', type=int, default=3)
    ap.add_argument('--out', default=str(ROOT / 'docs/bench-data'))
    a = ap.parse_args()
    for b in ('panpak', 'panpak-stub'):
        if not (BIN / b).exists():
            raise SystemExit(f'build the release first: {BIN / b} is missing')
    WORK.mkdir(parents=True, exist_ok=True)
    isolation.require_isolated_home(WORK / 'homes', WORK)
    for name in a.apps.split(','):
        for w in ('old', 'new'):
            if not inputs.path_of(APPS[name][w]).exists():
                raise SystemExit(f'missing input {APPS[name][w]}: python3 bench/inputs.py fetch')
    sway = Sway(WORK)
    rows = []
    try:
        for name in a.apps.split(','):
            rows += measure(name, APPS[name], sway, a.reps)
    finally:
        sway.close()
    header = ['app', 'arm', 'case', 'rep', 'ok', 'processes', 'unreadable', 'unreadable_rss_kib', 'rss_kib', 'pss_kib', 'pss_anon_kib', 'pss_file_kib', 'pss_shmem_kib', 'private_dirty_kib',
              'private_clean_kib', 'cg_current_kib', 'cg_anon_kib', 'cg_file_kib', 'cg_file_mapped_kib', 'cg_shmem_kib', 'memavailable_drop_kib']
    meta = pb.metadata()
    pb.write_csv(Path(a.out), 'shared-memory.csv', meta, header, rows)
    s = summary(rows)
    pb.write_csv(Path(a.out), 'shared-memory-summary.csv', meta, ['app', 'arm', 'new_alone_mib', 'old_alone_mib', 'both_mib', 'saved_mib', 'saved_percent', 'both_cgroup_mib'], s)
    for r in s:
        print({k: (round(v, 1) if isinstance(v, float) else v) for k, v in r.items()})


if __name__ == '__main__':
    main()
