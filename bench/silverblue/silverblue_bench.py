#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Measurements of step 9b: Panpak and Flatpak on Fedora Silverblue, one system per virtual machine.

Run it inside the virtual machine, as the user that is logged in to the GNOME session (not over ssh without the session bus: the window times come from the
session). Python 3.9 or newer and nothing else (no module outside the standard library).

  silverblue_bench.py check   --system flatpak|panpak
  silverblue_bench.py base    --system S                                                  what the system has before the series (the Fedora Flatpaks of Silverblue), versions
  silverblue_bench.py permissions --system S --app te                                     the permissions of the Text Editor (the one of Fedora, or ours), as the system says them
  silverblue_bench.py install --system S --app su|mj|qb [--pkg-url URL | --pkg-file F]     one install: bytes that came, space added, time
  silverblue_bench.py start   --system S --app A --kind warm --trials N                  N starts, one after the other (a start, a stop, a pause)
  silverblue_bench.py cycle   --system S --app A --trials R                              R times: remove, drop the caches, install, first start
  silverblue_bench.py scenario --system S --order te,qb --warm N                         one app after the other: install, first start, N warm starts
  silverblue_bench.py reboot-series start --system S --apps te,qb --count R              arm R reboots per app; each login measures one start and goes on
  silverblue_bench.py reboot-series cancel | status
  silverblue_bench.py summarize [--dir DIR]                                              tables from the result files

Every measurement is a line of JSON in DIR (default ~/panpak-bench-results/<system>.jsonl). Nothing is thrown away; `summarize` is the only thing that reduces.
The procedure, what each number means, and what to do before and after is in PROCEDURE.md.
"""
import argparse
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path

HOME = Path.home()
RESULTS = Path(os.environ.get('BENCH_RESULTS', HOME / 'panpak-bench-results'))
WINDOWS = HOME / '.cache/panpak-bench/windows.jsonl'
PANPAK = os.environ.get('PANPAK', str(HOME / '.local/bin/panpak'))

# The apps of the series: the same upstream version on both sides (PROCEDURE.md, section 2). `su` and `mj` are two GNOME apps on the same GNOME runtime (what Flatpak shares between
# two apps, and what Panpak shares as chunks), `qb` is a KDE app on another runtime. `te` is **not** in a series: it is the app of the test of permissions and operations (`permissions`),
# and on the Flatpak machine it is the Text Editor of Fedora that Silverblue already has, in the system installation.
APPS = {
    'su': {'name': 'GNOME Sudoku', 'flatpak': 'org.gnome.Sudoku', 'panpak_id': 'org.gnome.Sudoku', 'window': r'(?i)org\.gnome\.Sudoku|sudoku', 'series': True},
    'mj': {'name': 'GNOME Mahjongg', 'flatpak': 'org.gnome.Mahjongg', 'panpak_id': 'org.gnome.Mahjongg', 'window': r'(?i)org\.gnome\.Mahjongg|mahjongg', 'series': True},
    'qb': {'name': 'qBittorrent', 'flatpak': 'org.qbittorrent.qBittorrent', 'panpak_id': 'org.qbittorrent.qBittorrent', 'window': r'(?i)qbittorrent', 'series': True},
    'te': {'name': 'GNOME Text Editor', 'flatpak': 'org.gnome.TextEditor', 'panpak_id': 'org.gnome.TextEditor', 'window': r'(?i)org\.gnome\.TextEditor|texteditor', 'series': False},
}

# What each system adds to the disk, for the measure of the space: the places that it writes to.
SPACE_DIRS = {
    'flatpak': [HOME / '.local/share/flatpak', HOME / '.var/app'],
    'panpak': [HOME / '.local/share/panpak', HOME / '.cache/panpak'],
}


def say(*a):
    print(*a, flush=True)


def sh(cmd, check=True, timeout=1800, **kw):
    try:
        p = subprocess.run([str(c) for c in cmd], capture_output=True, text=True, timeout=timeout, **kw)
    except FileNotFoundError:
        # A tool that is not there (`rpm-ostree` on another system, `gnome-shell` over ssh) is an answer, not a crash: 127, as a shell says it.
        p = subprocess.CompletedProcess([str(c) for c in cmd], 127, '', f'{cmd[0]}: not found\n')
    if check and p.returncode != 0:
        raise SystemExit(f'failed: {" ".join(map(str, cmd))}\n{p.stdout}{p.stderr}')
    return p


# ---------------------------------------------------------------------------------------------------------------------------------------------- bytes and space

def default_iface():
    for line in Path('/proc/net/route').read_text().splitlines()[1:]:
        f = line.split()
        if len(f) > 2 and f[1] == '00000000':
            return f[0]
    raise SystemExit('no default route: the virtual machine has no network, and the bytes that come cannot be counted')


def has_route():
    try:
        default_iface()
        return True
    except SystemExit:
        return False


def rx_bytes():
    """Bytes received on the interface of the default route, from the kernel counter. Compared before and after a command, with nothing else running."""
    want = default_iface()
    for line in Path('/proc/net/dev').read_text().splitlines():
        if ':' in line and line.split(':', 1)[0].strip() == want:
            return int(line.split(':', 1)[1].split()[0])
    raise SystemExit(f'interface {want} not found in /proc/net/dev')


def du(path):
    """Bytes of disk that a folder uses (blocks, as `du` counts them; a file that is linked twice in the folder counts once)."""
    if not Path(path).exists():
        return 0
    p = sh(['du', '-sx', '--block-size=1', path], check=False)
    return int(p.stdout.split()[0]) if p.stdout.strip() else 0


def space(system):
    return {str(d): du(d) for d in SPACE_DIRS[system]}


def fs_used():
    """Bytes used on the file system of the home, from the file system itself (a cross-check of the folders above)."""
    st = os.statvfs(HOME)
    return (st.f_blocks - st.f_bfree) * st.f_frsize


# The two commands that the script runs as root, written as the sudoers rule of PROCEDURE.md allows them: the whole command line, no more.
SYSCTL_DROP = ['/usr/sbin/sysctl', '-q', 'vm.drop_caches=3']
REBOOT = ['/usr/bin/systemctl', 'reboot']


def drop_caches():
    """The page cache, the dentries and the inodes, so that the next start reads from the disk. Needs the sudoers rule of PROCEDURE.md (section 1)."""
    sh(['sync'])
    p = sh(['sudo', '-n', *SYSCTL_DROP], check=False)
    if p.returncode != 0:
        raise SystemExit('cannot drop the caches: the sudoers rule of PROCEDURE.md (section 1) is not there, or it does not name ' + ' '.join(SYSCTL_DROP))


def quiet(seconds=5):
    """Let what the system does in the background settle, so that the byte counter and the disk are not moving."""
    time.sleep(seconds)


# ---------------------------------------------------------------------------------------------------------------------------------------------- results

def out_path(system):
    RESULTS.mkdir(parents=True, exist_ok=True)
    return RESULTS / f'{system}.jsonl'


def record(system, **fields):
    row = {'system': system, 'at': time.time(), **fields}
    with out_path(system).open('a') as f:
        f.write(json.dumps(row) + '\n')
    say(json.dumps(row))
    return row


# ---------------------------------------------------------------------------------------------------------------------------------------------- the systems

def ensure_flathub():
    p = sh(['flatpak', 'remotes', '--user', '--columns=name'], check=False)
    if 'flathub' not in p.stdout.split():
        sh(['flatpak', 'remote-add', '--user', '--if-not-exists', 'flathub', 'https://dl.flathub.org/repo/flathub.flatpakrepo'])


def launcher(system, app):
    a = APPS[app]
    if system == 'flatpak':
        return ['flatpak', 'run', a['flatpak']]
    return [str(HOME / '.local/bin' / a['panpak_id'])]


def install_cmds(system, app, pkg_url, pkg_file, keys=None):
    """The steps of an install, in the order they run. A step is (what, command)."""
    a = APPS[app]
    if not a['series']:
        raise SystemExit(f'{app} is not an app of the series (it is the app of the test of permissions and operations): nothing is installed or measured for it')
    if system == 'flatpak':
        ensure_flathub()
        return [('install', ['flatpak', 'install', '--user', '-y', '--noninteractive', 'flathub', a['flatpak']])]
    steps = []
    f = pkg_file
    if pkg_url:
        f = str(HOME / '.cache/panpak-bench' / f'{a["panpak_id"]}.panpak')
        Path(f).parent.mkdir(parents=True, exist_ok=True)
        steps.append(('download', ['curl', '-fsS', '-o', f, pkg_url]))
    if not f:
        raise SystemExit('panpak: give --pkg-url (counted as bytes that come) or --pkg-file (a package that is already here)')
    # The key that the author published, given by the person (`--keys te=ed25519:...`): the install refuses a package of another key and pins this one. Never `--trust-first-key`: the
    # safe way is the one that the comparison shows.
    key = (keys or {}).get(app)
    if not key:
        raise SystemExit(f'panpak: give the key that the author of {app} published (--keys {app}=ed25519:...); the install is made with --expect-key, not --trust-first-key')
    steps.append(('install', [PANPAK, 'install', '--yes', '--expect-key', key, f]))
    return steps


def parse_keys(text):
    """`te=ed25519:<hex>,qb=ed25519:<hex>` as a dictionary; a key that does not have the form of a key is refused here, before anything is downloaded."""
    keys = {}
    for part in (text or '').split(','):
        if not part.strip():
            continue
        app, _, key = part.partition('=')
        if app not in APPS or not re.fullmatch(r'ed25519:[0-9a-f]{64}', key):
            raise SystemExit(f'--keys: {part!r} is not <app>=ed25519:<64 lowercase hex> with an app of {sorted(APPS)}')
        keys[app] = key
    return keys


def uninstall(system, app):
    a = APPS[app]
    if system == 'flatpak':
        sh(['flatpak', 'uninstall', '--user', '-y', '--noninteractive', '--delete-data', a['flatpak']], check=False)
        sh(['flatpak', 'uninstall', '--user', '-y', '--noninteractive', '--unused'], check=False)
    else:
        sh([PANPAK, 'remove', '--purge-data', '--yes', a['panpak_id']], check=False)
        sh([PANPAK, 'gc'], check=False)
        pkg = HOME / '.cache/panpak-bench' / f'{a["panpak_id"]}.panpak'
        if pkg.exists():
            pkg.unlink()


def do_install(system, app, args, tag=None):
    steps = install_cmds(system, app, getattr(args, 'pkg_url', None), getattr(args, 'pkg_file', None), parse_keys(getattr(args, 'keys', None)))
    quiet()
    space0, used0, rx0, t0 = space(system), fs_used(), rx_bytes(), time.time()
    times = {}
    for what, cmd in steps:
        s = time.time()
        sh(cmd)
        times[what + '_s'] = round(time.time() - s, 3)
    wall = time.time() - t0
    quiet()
    rx1, space1, used1 = rx_bytes(), space(system), fs_used()
    return record(system, kind='install', app=app, tag=tag, wall_s=round(wall, 3), **times, rx_bytes=rx1 - rx0,
                  space_bytes=sum(space1.values()) - sum(space0.values()), fs_used_bytes=used1 - used0,
                  space_dirs={k: space1[k] - space0[k] for k in space1})


# ---------------------------------------------------------------------------------------------------------------------------------------------- the windows

def window_events():
    if not WINDOWS.exists():
        return []
    rows = []
    for line in WINDOWS.read_text().splitlines():
        try:
            rows.append(json.loads(line))
        except ValueError:
            pass
    return rows


def extension_ready():
    return WINDOWS.exists() and any(r.get('event') == 'enabled' for r in window_events())


def wait_frame(app, since, timeout):
    """The time (seconds since 1970) of the first frame of a window of `app` that appeared after `since`, or None."""
    pat = re.compile(APPS[app]['window'])
    end = time.time() + timeout
    while time.time() < end:
        for r in window_events():
            if r.get('t', 0) >= since and r.get('event') == 'first-frame' and (pat.search(r.get('wm_class', '')) or pat.search(r.get('app_id', ''))):
                return r['t']
        time.sleep(0.02)
    return None


def stop(system, app, unit):
    """End the app and everything it started, and wait until its window is gone."""
    if system == 'flatpak':
        sh(['flatpak', 'kill', APPS[app]['flatpak']], check=False)
    sh(['systemctl', '--user', 'stop', f'{unit}.scope'], check=False)
    time.sleep(1.0)


def one_start(system, app, kind, trial, timeout=60):
    """One start: the command is started inside a systemd scope (for both systems, so that the cost is the same) and stopped through it. The time is from just before the
    command to the first frame of the window that the compositor reports."""
    if not APPS[app]['series']:
        raise SystemExit(f'{app} is not an app of the series: no time is measured for it (PROCEDURE.md, section 6 is the test of permissions and operations)')
    if not extension_ready():
        raise SystemExit('the window extension is not running: PROCEDURE.md, section 1 (install winwatch@panpak.bench, enable it, log in again)')
    unit = f'panpak-bench-{os.getpid()}-{trial}'
    cmd = ['systemd-run', '--user', '--scope', '--quiet', f'--unit={unit}', '--', *launcher(system, app)]
    t0 = time.time()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    t = wait_frame(app, t0, timeout)
    stop(system, app, unit)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)  # the group that this script made, never by a name
    if t is None:
        return record(system, kind=kind, app=app, trial=trial, ok=False, note='no window within %d s' % timeout)
    return record(system, kind=kind, app=app, trial=trial, ok=True, window_s=round(t - t0, 4))


def warm_starts(system, app, n, gap=3.0, kind='warm'):
    for i in range(n):
        one_start(system, app, kind, i)
        time.sleep(gap)


# ---------------------------------------------------------------------------------------------------------------------------------------------- the commands

def cmd_check(args):
    ok = True
    for what, cond in [
        ('the window extension has written "enabled"', extension_ready()),
        ('a default route (the bytes that come can be counted)', has_route()),
        ('the sudoers rule for the caches (and only for the caches and the reboot)', sh(['sudo', '-n', '-l', *SYSCTL_DROP], check=False).returncode == 0 and sh(['sudo', '-n', '-l', *REBOOT], check=False).returncode == 0),
        ('systemd-run --user works', sh(['systemd-run', '--user', '--scope', '--quiet', '--', 'true'], check=False).returncode == 0),
        ('the tool of the system is there', sh(['which', 'flatpak' if args.system == 'flatpak' else PANPAK], check=False).returncode == 0),
    ]:
        say(('ok    ' if cond else 'FAIL  ') + what)
        ok &= bool(cond)
    say('session type: ' + os.environ.get('XDG_SESSION_TYPE', '?') + ', desktop: ' + os.environ.get('XDG_CURRENT_DESKTOP', '?'))
    say('gnome-shell: ' + sh(['gnome-shell', '--version'], check=False).stdout.strip())
    sys.exit(0 if ok else 1)


def cmd_base(args):
    """What the system already has before anything is installed: the apps and runtimes of Fedora that Silverblue brings (Flatpaks of the system installation), their space, the system
    and the versions. It is part of what a person receives, so it is written down and not hidden: it is the same on both machines (clones of one image) and the series start from it."""
    fl = sh(['flatpak', 'list', '--system', '--columns=application,branch,size,origin'], check=False).stdout
    sysdir = Path('/var/lib/flatpak')
    row = record(args.system, kind='base', app='-', flatpak_system_bytes=du(sysdir), flatpak_system_apps_bytes=du(sysdir / 'app'), flatpak_system_runtimes_bytes=du(sysdir / 'runtime'),
                 fs_used_bytes=fs_used(), flatpak_system_list=fl.strip().splitlines(),
                 versions={'uname': os.uname().release, 'ldd': sh(['ldd', '--version'], check=False).stdout.splitlines()[:1], 'gnome_shell': sh(['gnome-shell', '--version'], check=False).stdout.strip(),
                           'flatpak': sh(['flatpak', '--version'], check=False).stdout.strip(), 'rpm_ostree': sh(['rpm-ostree', 'status'], check=False).stdout.splitlines()[:8]})
    RESULTS.mkdir(parents=True, exist_ok=True)
    (RESULTS / f'base-{args.system}.txt').write_text(fl)
    return row


def cmd_permissions(args):
    """The permissions that a system gives an app, as the system says them, for the test of permissions and operations (PROCEDURE.md, section 6): `flatpak info --show-permissions`
    for the Text Editor of Fedora that is already on the Flatpak machine, `panpak inspect` (text and JSON) for ours."""
    a = APPS[args.app]
    RESULTS.mkdir(parents=True, exist_ok=True)
    out = RESULTS / f'permissions-{args.system}-{args.app}.txt'
    if args.system == 'flatpak':
        parts = [('flatpak info', ['flatpak', 'info', a['flatpak']]), ('flatpak info --show-permissions', ['flatpak', 'info', '--show-permissions', a['flatpak']]),
                 ('flatpak info --show-metadata', ['flatpak', 'info', '--show-metadata', a['flatpak']])]
    else:
        parts = [('panpak inspect', [PANPAK, 'inspect', a['panpak_id']]), ('panpak inspect --json', [PANPAK, 'inspect', '--json', a['panpak_id']])]
    text = ''
    for title, cmd in parts:
        p = sh(cmd, check=False)
        text += f'## {title}\n{p.stdout}{p.stderr}\n'
    out.write_text(text)
    record(args.system, kind='permissions', app=args.app, file=out.name)


def cmd_install(args):
    do_install(args.system, args.app, args)


def cmd_start(args):
    warm_starts(args.system, args.app, args.trials, args.gap, args.kind)


def cmd_cycle(args):
    for i in range(args.trials):
        uninstall(args.system, args.app)
        drop_caches()
        quiet()
        do_install(args.system, args.app, args, tag=f'cycle{i}')
        one_start(args.system, args.app, 'first', i)


def cmd_scenario(args):
    """One app after the other, as a person would: the first app installed with nothing of the system on the disk, then the second."""
    for position, app in enumerate(args.order.split(',')):
        drop_caches()
        if args.system == 'panpak':
            args.pkg_url = f'{args.pkg_base_url.rstrip("/")}/{APPS[app]["panpak_id"]}.panpak'
        do_install(args.system, app, args, tag=f'position{position}')
        one_start(args.system, app, 'first', 0)
        warm_starts(args.system, app, args.warm, args.gap)


STATE = RESULTS / 'reboot-state.json'
UNIT = HOME / '.config/systemd/user/panpak-bench-reboot.service'


def cmd_reboot(args):
    if args.action == 'cancel':
        sh(['systemctl', '--user', 'disable', 'panpak-bench-reboot.service'], check=False)
        STATE.unlink(missing_ok=True)
        return
    if args.action == 'status':
        say(STATE.read_text() if STATE.exists() else 'no series is armed')
        return
    if args.action == 'start':
        apps = args.apps.split(',')
        RESULTS.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps({'system': args.system, 'apps': apps, 'total': args.count * len(apps), 'done': 0, 'settle': args.settle}))
        UNIT.parent.mkdir(parents=True, exist_ok=True)
        UNIT.write_text(f'[Unit]\nDescription=Measure one start after a reboot\nAfter=graphical-session.target\n\n[Service]\nType=oneshot\n'
                        f'Environment=PYTHONUNBUFFERED=1\nExecStart={sys.executable} {os.path.abspath(__file__)} reboot-series step\n\n[Install]\nWantedBy=default.target\n')
        sh(['systemctl', '--user', 'daemon-reload'])
        sh(['systemctl', '--user', 'enable', 'panpak-bench-reboot.service'])
        say('armed: the next login measures one start and reboots again until the series is done. Reboot now with `systemctl reboot`.')
        return
    # step: run by the unit at login
    st = json.loads(STATE.read_text())
    if st['done'] >= st['total']:
        sh(['systemctl', '--user', 'disable', 'panpak-bench-reboot.service'], check=False)
        return
    end = time.time() + 180
    while not extension_ready() and time.time() < end:
        time.sleep(2)
    # Boot and login keep the machine busy for a while: the measure starts after a fixed time of quiet, the same in every series.
    time.sleep(st['settle'])
    app = st['apps'][st['done'] % len(st['apps'])]
    one_start(st['system'], app, 'after-reboot', st['done'])
    st['done'] += 1
    STATE.write_text(json.dumps(st))
    if st['done'] < st['total']:
        time.sleep(5)
        if sh(['systemctl', 'reboot'], check=False).returncode != 0:  # polkit lets the user of the session do it; the sudoers rule is the fallback
            sh(['sudo', '-n', *REBOOT])
    else:
        sh(['systemctl', '--user', 'disable', 'panpak-bench-reboot.service'], check=False)
        say('series done')


# ---------------------------------------------------------------------------------------------------------------------------------------------- the tables

def quartiles(xs):
    xs = sorted(xs)
    if len(xs) < 2:
        return xs[0], xs[0]
    q = statistics.quantiles(xs, n=4, method='inclusive')
    return q[0], q[2]


def stats(xs):
    xs = [float(x) for x in xs]
    med = statistics.median(xs)
    q1, q3 = quartiles(xs)
    mad = statistics.median([abs(x - med) for x in xs])
    return {'n': len(xs), 'median': med, 'min': min(xs), 'max': max(xs), 'q1': q1, 'q3': q3, 'mad': mad, 'mad_pct': (100 * mad / med) if med else 0.0}


def mib(x):
    return f'{x / 1048576:.1f}'


def load_rows(dirs):
    """The rows of every `*.jsonl` of the folders. The name of the folder is the order of the apps of that run (`te-qb`, `qb-te`)."""
    rows = []
    for d in dirs:
        for f in sorted(Path(d).glob('*.jsonl')):
            for line in f.read_text().splitlines():
                if line.strip():
                    r = json.loads(line)
                    r['order'] = Path(d).name
                    rows.append(r)
    return rows


def cmd_summarize(args):
    rows = load_rows(args.dir)
    if not rows:
        raise SystemExit(f'no results in {" ".join(args.dir)}')
    groups = {}
    for r in rows:
        key = (r['order'], r['system'], r['app'], r['kind'])
        groups.setdefault(key, []).append(r)
    say('### Time to the first frame of the window (seconds)\n')
    say('| order | system | app | kind | n | median | min | max | Q1 | Q3 | MAD (% of median) | failed |')
    say('|---|---|---|---|---|---|---|---|---|---|---|---|')
    for (order, system, app, kind), rs in sorted(groups.items()):
        if kind == 'install':
            continue
        ok = [r['window_s'] for r in rs if r.get('ok')]
        bad = sum(1 for r in rs if not r.get('ok'))
        if not ok:
            say(f'| {order} | {system} | {app} | {kind} | 0 | | | | | | | {bad} |')
            continue
        s = stats(ok)
        say(f'| {order} | {system} | {app} | {kind} | {s["n"]} | {s["median"]:.3f} | {s["min"]:.3f} | {s["max"]:.3f} | {s["q1"]:.3f} | {s["q3"]:.3f} | {s["mad_pct"]:.1f} | {bad} |')
    say('\n### Installs: bytes that came, space added, time\n')
    say('| order | system | app | tag | n | MiB that came (median) | MiB on disk (median) | MiB on disk (min-max) | seconds (median) |')
    say('|---|---|---|---|---|---|---|---|---|')
    inst = {}
    for r in rows:
        if r['kind'] == 'install':
            inst.setdefault((r['order'], r['system'], r['app'], (r.get('tag') or '').rstrip('0123456789') or '-'), []).append(r)
    for (order, system, app, tag), rs in sorted(inst.items()):
        rx, sp, wall = stats([r['rx_bytes'] for r in rs]), stats([r['space_bytes'] for r in rs]), stats([r['wall_s'] for r in rs])
        say(f'| {order} | {system} | {app} | {tag} | {rx["n"]} | {mib(rx["median"])} | {mib(sp["median"])} | {mib(sp["min"])}-{mib(sp["max"])} | {wall["median"]:.1f} |')
    say('\nThe tag `position0` is the first app of a scenario, `position1` the second: the second one shows what a system shares between two apps.')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)

    def common(p, apps=True):
        p.add_argument('--system', choices=['flatpak', 'panpak'], required=True)
        if apps:
            p.add_argument('--app', choices=sorted(APPS), required=True)

    def pkg(p):
        p.add_argument('--pkg-url', help='panpak: where the package is served (the bytes that come are counted)')
        p.add_argument('--pkg-file', help='panpak: a package that is here already (nothing is counted)')
        p.add_argument('--keys', help='panpak: the key that the author of each app published, as te=ed25519:<hex>,qb=ed25519:<hex>; the install uses --expect-key with it')

    p = sub.add_parser('check')
    common(p, apps=False)
    p.set_defaults(fn=cmd_check)
    p = sub.add_parser('base')
    p.add_argument('--system', choices=['flatpak', 'panpak'], required=True)
    p.set_defaults(fn=cmd_base)
    p = sub.add_parser('permissions')
    common(p)
    p.set_defaults(fn=cmd_permissions)
    p = sub.add_parser('install')
    common(p)
    pkg(p)
    p.set_defaults(fn=cmd_install)
    p = sub.add_parser('start')
    common(p)
    p.add_argument('--kind', default='warm')
    p.add_argument('--trials', type=int, default=15)
    p.add_argument('--gap', type=float, default=3.0)
    p.set_defaults(fn=cmd_start)
    p = sub.add_parser('cycle')
    common(p)
    pkg(p)
    p.add_argument('--trials', type=int, default=5)
    p.set_defaults(fn=cmd_cycle)
    p = sub.add_parser('scenario')
    common(p, apps=False)
    p.add_argument('--order', required=True, help='the apps in the order they are installed: su,mj,qb or qb,mj,su (panpak needs --pkg-base-url and --keys)')
    p.add_argument('--warm', type=int, default=15)
    p.add_argument('--gap', type=float, default=3.0)
    p.add_argument('--pkg-base-url', help='panpak: the folder that serves <app id>.panpak')
    p.add_argument('--keys', help='panpak: te=ed25519:<hex>,qb=ed25519:<hex> (the key that each author published; --expect-key)')
    p.set_defaults(fn=cmd_scenario)
    p = sub.add_parser('reboot-series')
    p.add_argument('action', choices=['start', 'step', 'cancel', 'status'])
    p.add_argument('--system', choices=['flatpak', 'panpak'])
    p.add_argument('--apps', default='su,mj,qb')
    p.add_argument('--count', type=int, default=5, help='reboots per app')
    p.add_argument('--settle', type=int, default=45, help='seconds of quiet after the extension is up, before the start is measured')
    p.set_defaults(fn=cmd_reboot)
    p = sub.add_parser('summarize')
    p.add_argument('--dir', nargs='+', default=[str(RESULTS)], help='one folder per run; its name is shown as the order of the apps')
    p.set_defaults(fn=cmd_summarize)
    args = ap.parse_args()
    if args.cmd == 'scenario' and args.system == 'panpak':
        if not args.pkg_base_url:
            ap.error('panpak: --pkg-base-url is needed')
    if args.cmd == 'reboot-series' and args.action == 'start' and not args.system:
        ap.error('--system is needed')
    args.fn(args)


if __name__ == '__main__':
    main()
