#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Measurements of step 9b: Panpak and Flatpak on Fedora Silverblue, one system per virtual machine.

Run it inside the virtual machine, as the user that is logged in to the GNOME session (not over ssh without the session bus: the window times come from the
session). Python 3.9 or newer and nothing else (no module outside the standard library).

  silverblue_bench.py check   --system flatpak|panpak
  silverblue_bench.py base    --system S                                                  what the system has before the series (the Fedora Flatpaks of Silverblue), versions
  silverblue_bench.py update  --system S --app A --trials 3                                the update from the previous version (Flathub commit, or the .prev.panpak of the conversion)
  silverblue_bench.py sandbox --system S --app onlyoffice                                  the processes of an app with a browser engine, as the system sandboxes them
  silverblue_bench.py permissions --system S --app te                                     the permissions of the Text Editor (the one of Fedora, or ours), as the system says them
  silverblue_bench.py install --system S --app gimp|kdenlive|onlyoffice|qb [--pkg-url URL | --pkg-file F]     one install: bytes that came, space added, time
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
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HOME = Path.home()
RESULTS = Path(os.environ.get('BENCH_RESULTS', HOME / 'panpak-bench-results'))
WINDOWS = HOME / '.cache/panpak-bench/windows.jsonl'
RUNTIMES = HOME / '.local/share/panpak/runtimes'  # where the panpak-stub (the runtime of the launcher) is installed
PANPAK = os.environ.get('PANPAK', str(HOME / '.local/bin/panpak'))

# The apps of the series (PROCEDURE.md, section 2): daily-use apps that a person adds to Silverblue from Flathub, at the same upstream version as the official AppImage that the Panpak package is
# converted from (a conversion made by us, not a package of the author), and qBittorrent, built from source, as the example of the build pipeline (it shares the KDE 6.11 runtime of Kdenlive).
# `te` is **not** in a series: it is the app of the test of permissions and operations (`permissions`), and on the Flatpak machine it is the Text Editor of Fedora that Silverblue already has.
# `update`: the Flathub commit of the previous version (the history of Flathub keeps only a few commits, so a previous version may not be there: then there is no pair) and the versions.
APPS = {
    'gimp': {'name': 'GIMP', 'flatpak': 'org.gimp.GIMP', 'panpak_id': 'org.gimp.GIMP', 'window': r'(?i)org\.gimp\.GIMP|gimp', 'title': r'Image Manipulation Program', 'series': True, 'update': None},
    'kdenlive': {'name': 'Kdenlive', 'flatpak': 'org.kde.kdenlive', 'panpak_id': 'org.kde.kdenlive', 'window': r'(?i)org\.kde\.kdenlive|kdenlive', 'title': r'^(?!Splash).+ \u2014 Kdenlive$', 'series': True,
                 'update': {'commit': 'b1fc34accc861c296d08863b01c65d9c32816bdf1d08414ee157de565e56199d', 'from': '26.08.2', 'to': '26.08.3'}},
    'onlyoffice': {'name': 'ONLYOFFICE Desktop Editors', 'flatpak': 'org.onlyoffice.desktopeditors', 'panpak_id': 'org.onlyoffice.desktopeditors', 'window': r'(?i)onlyoffice|desktopeditors', 'title': r'ONLYOFFICE', 'series': True,
                   'update': {'commit': '5806f4728bd3e2bc5314c2c0dfbc59d10dcac33516b5536ac12ceae2e79f98c1', 'from': '9.4.0', 'to': '10.0.0'}},
    'qb': {'name': 'qBittorrent', 'flatpak': 'org.qbittorrent.qBittorrent', 'panpak_id': 'org.qbittorrent.qBittorrent', 'window': r'(?i)qbittorrent', 'title': r'qBittorrent', 'series': True, 'update': None},
    'te': {'name': 'GNOME Text Editor', 'flatpak': 'org.gnome.TextEditor', 'panpak_id': 'org.gnome.TextEditor', 'window': r'(?i)org\.gnome\.TextEditor|texteditor', 'series': False, 'update': None},
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
    if os.environ.get('BENCH_SKIP_DROP_CACHES') == '1':
        say('NOTE: BENCH_SKIP_DROP_CACHES=1: the caches are not dropped (a rehearsal on a machine without the sudoers rule; times of a first start are not cold)')
        return
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


def install_cmds(system, app, pkg_url, pkg_file, keys=None, previous=False):
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
        suffix = '.prev.panpak' if previous else '.panpak'
        f = str(HOME / '.cache/panpak-bench' / f'{a["panpak_id"]}{suffix}')
        Path(f).parent.mkdir(parents=True, exist_ok=True)
        ca = os.environ.get('BENCH_CA')
        steps.append(('download', ['curl', '-fsS', *(['--cacert', ca] if ca else []), '-o', f, pkg_url]))
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
    """`gimp=ed25519:<hex>,qb=ed25519:<hex>` (or `*=ed25519:<hex>` for every app: the conversions are all signed with one key, the key of the converter) as a dictionary; a key that does
    not have the form of a key is refused here, before anything is downloaded."""
    keys = {}
    for part in (text or '').split(','):
        if not part.strip():
            continue
        app, _, key = part.partition('=')
        if (app != '*' and app not in APPS) or not re.fullmatch(r'ed25519:[0-9a-f]{64}', key):
            raise SystemExit(f'--keys: {part!r} is not <app>=ed25519:<64 lowercase hex> (an app of {sorted(APPS)}, or * for all)')
        for name in (APPS if app == '*' else [app]):
            keys[name] = key
    return keys


def uninstall(system, app):
    a = APPS[app]
    if system == 'flatpak':
        sh(['flatpak', 'uninstall', '--user', '-y', '--noninteractive', '--delete-data', a['flatpak']], check=False)
        sh(['flatpak', 'uninstall', '--user', '-y', '--noninteractive', '--unused'], check=False)
        # Some Flatpaks keep their settings outside ~/.var/app, in a folder of the home that the manifest asks for (GIMP: xdg-config/GIMP): `--delete-data` does not remove it, and a second trial would not
        # be a first start. It is removed with the data (the machines of the series have no other use for it).
        for rel in FLATPAK_HOME_DATA.get(app, []):
            shutil.rmtree(HOME / rel, ignore_errors=True)
    else:
        sh([PANPAK, 'remove', '--purge-data', '--yes', a['panpak_id']], check=False)
        sh([PANPAK, 'gc'], check=False)
        for name in (f'{a["panpak_id"]}.panpak', f'{a["panpak_id"]}.prev.panpak'):
            pkg = HOME / '.cache/panpak-bench' / name
            if pkg.exists():
                pkg.unlink()


# Folders of the home, outside ~/.var/app, where a Flatpak keeps the app's settings (a permission of its manifest).
FLATPAK_HOME_DATA = {'gimp': ['.config/GIMP']}


# What a person does once, at the very first start, and is not part of the start that is measured: qBittorrent shows a modal legal notice before its main window, until it is accepted (a setting of the
# app, `LegalNotice\\Accepted`). It is written for the two systems in the place where each keeps the app's configuration, after the install has been measured and before the first start.
FIRST_RUN_CONFIG = {
    # app: (file in the configuration folder under Flatpak, file under Panpak, text added to it)
    'qb': ('qBittorrent/qBittorrent.conf', 'qBittorrent/qBittorrent.conf', '[LegalNotice]\nAccepted=true\n'),
    # GIMP shows a welcome window after the main one, at every start of a profile that has not set `show-welcome-dialog`; with no `config-version` in the file it takes the profile for a new one and shows it anyway,
    # so the two lines go together. (Its Flatpak keeps this file in ~/.config/GIMP, a permission of its manifest; under Panpak it is in the private home of the app.)
    'gimp': ('GIMP/3.2/gimprc', 'GIMP/3.2/gimprc', '(config-version "3.2.8")\n(show-welcome-dialog no)\n'),
    # Kdenlive: a first start shows a window that is not the main one: the quick settings (when the group `[version]` is not in its file yet) and then the welcome page (`showWelcome`). What a confirmed
    # first run writes is the text below (read from a real one); the Flatpak of KDE keeps the file as `kdenlive-flatpakrc`, with the paths of its own build under `[env]`. For Panpak the `[env]` paths are
    # the ones that a confirmed first run writes under its view (see PROCEDURE.md): they are not guessed here.
    'kdenlive': ('kdenlive-flatpakrc', 'kdenliverc', '[Recent Profiles]\nrecentProfileNames=HD 1080p 25 fps\nrecentProfiles=atsc_1080p_25\n\n'
                 '[capture]\ndecklink_extension=mov\ngrab_extension=mov\ngrab_parameters=-crf 25 -vcodec libx264 -preset veryfast -threads 0\n\n'
                 '[startup]\nshowWelcome=false\nlastSeenVersionMajor=26\nlastSeenVersionMicro=3\nlastSeenVersionMinor=8\n\n[version]\nversion=26.08.3\n'),
}


def seed_first_run(system, app):
    if app not in FIRST_RUN_CONFIG:
        return
    flat, pan, text = FIRST_RUN_CONFIG[app]
    if system == 'flatpak' and app in FLATPAK_HOME_DATA:
        base = HOME / '.config'  # GIMP's Flatpak reads and writes ~/.config/GIMP (xdg-config/GIMP:create)
    else:
        base = HOME / '.var/app' / APPS[app]['flatpak'] / 'config' if system == 'flatpak' else HOME / '.local/share/panpak/userdata' / APPS[app]['panpak_id'] / '.config'
    path = base / (flat if system == 'flatpak' else pan)
    path.parent.mkdir(parents=True, exist_ok=True)
    old = path.read_text() if path.exists() else ''
    if text not in old:
        path.write_text(old + ('\n' if old and not old.endswith('\n') else '') + text)


def do_install(system, app, args, tag=None):
    steps = install_cmds(system, app, getattr(args, 'pkg_url', None), getattr(args, 'pkg_file', None), parse_keys(getattr(args, 'keys', None)))
    quiet()
    rt0 = du(RUNTIMES) if system == 'panpak' else None
    space0, used0, rx0, t0 = space(system), fs_used(), rx_bytes(), time.time()
    times = {}
    for what, cmd in steps:
        s = time.time()
        sh(cmd)
        times[what + '_s'] = round(time.time() - s, 3)
    wall = time.time() - t0
    quiet()
    rx1, space1, used1 = rx_bytes(), space(system), fs_used()
    extra = {}
    if system == 'panpak':
        # Did this install put the runtime of the launcher (the panpak-stub) in place by itself? (step 10a: the user never has to run install-runtime by hand)
        rt1 = du(RUNTIMES)
        extra = {'runtime_bytes_before': rt0, 'runtime_bytes_after': rt1, 'runtime_installed': rt0 == 0 and rt1 > 0}
    row = record(system, kind='install', app=app, tag=tag, wall_s=round(wall, 3), **times, **extra, rx_bytes=rx1 - rx0,
                  space_bytes=sum(space1.values()) - sum(space0.values()), fs_used_bytes=used1 - used0,
                  space_dirs={k: space1[k] - space0[k] for k in space1})
    seed_first_run(system, app)
    return row


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


def is_main_window(app, r):
    """True if the line is the first frame of the main window of `app`: the window that the person works in, never a screen of start. The window is the one of the app (`window`: its class or id) and, where the
    app opens a screen of start first, its title is the one of the main window (`title`). The same rule for Flatpak and for Panpak (PROCEDURE.md, "The window that counts")."""
    a = APPS[app]
    if r.get('event') != 'first-frame':
        return False
    if not (re.search(a['window'], r.get('wm_class', '')) or re.search(a['window'], r.get('app_id', ''))):
        return False
    return not a.get('title') or re.search(a['title'], r.get('title', '')) is not None


def wait_frame_row(app, since, timeout):
    """The whole line of the first frame of the main window of `app` after `since` (it has the process id of the client that owns the window), or None."""
    end = time.time() + timeout
    while time.time() < end:
        for r in window_events():
            if r.get('t', 0) >= since and is_main_window(app, r):
                return r
        time.sleep(0.02)
    return None


def wait_frame(app, since, timeout):
    """The time (seconds since 1970) of the first frame of the main window of `app` that appeared after `since`, or None."""
    r = wait_frame_row(app, since, timeout)
    return r['t'] if r else None


def stop(system, app, unit):
    """End the app and everything it started, and wait until its window is gone."""
    if system == 'flatpak':
        sh(['flatpak', 'kill', APPS[app]['flatpak']], check=False)
    sh(['systemctl', '--user', 'stop', f'{unit}.scope'], check=False)
    time.sleep(1.0)


def one_start(system, app, kind, trial, timeout=60, hold=0.0):
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
    row = wait_frame_row(app, t0, timeout)
    t = row['t'] if row else None
    if row and hold:
        time.sleep(hold)  # for a person who watches: the window stays on the screen
    stop(system, app, unit)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)  # the group that this script made, never by a name
    if t is None:
        return record(system, kind=kind, app=app, trial=trial, ok=False, note='no window within %d s' % timeout)
    return record(system, kind=kind, app=app, trial=trial, ok=True, window_s=round(t - t0, 4), window_title=row.get('title', ''))


def warm_starts(system, app, n, gap=3.0, kind='warm', hold=0.0):
    for i in range(n):
        one_start(system, app, kind, i, hold=hold)
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


def import_session_env():
    """The display of the graphical session, for a script that is not started by it (ssh, a service): the apps that have only an X11 plugin (ONLYOFFICE) need DISPLAY and XAUTHORITY, and every app WAYLAND_DISPLAY.
    GNOME puts them in the environment of the user manager; the ones already set are kept."""
    p = sh(['systemctl', '--user', 'show-environment'], check=False)
    for line in (p.stdout or '').splitlines():
        k, _, v = line.partition('=')
        if k in ('DISPLAY', 'XAUTHORITY', 'WAYLAND_DISPLAY', 'XDG_SESSION_TYPE', 'XDG_CURRENT_DESKTOP') and v and k not in os.environ:
            os.environ[k] = v


def apply_ca(args):
    """`--ca FILE`: the authority that signed the certificate of the host that serves the packages (`serve_https.py`). Used by `curl` for the download and, for Panpak, by `network.ca_file` of its settings
    (the update that Panpak makes from the address in the manifest goes over HTTPS to the same host)."""
    ca = getattr(args, 'ca', None)
    if ca:
        os.environ['BENCH_CA'] = str(Path(ca).resolve())
        if getattr(args, 'system', None) == 'panpak':
            sh([PANPAK, 'config', 'set', 'network.ca_file', os.environ['BENCH_CA']])


def flatpak_installed(ref):
    return sh(['flatpak', 'info', '--user', ref], check=False).returncode == 0


def cmd_update(args):
    """The update of an app from the previous version to the one of the series, measured as the person who gets it: for Flatpak, `flatpak update` from the commit of Flathub of the previous version to the
    latest one; for Panpak, `panpak update` from the converted package of the previous version to the one at the address in its manifest (the chunks that the installed version does not have, by
    range requests). The preparation (the previous version, installed) is not measured. A previous version that is not in both formats has no pair: it is refused, not invented."""
    a = APPS[args.app]
    u = a['update']
    if not u:
        raise SystemExit(f'{args.app}: there is no previous version in both formats (the history of Flathub does not keep it): no update is measured for it')
    for i in range(args.trials):
        uninstall(args.system, args.app)
        drop_caches()
        if args.system == 'flatpak':
            ensure_flathub()
            sh(['flatpak', 'install', '--user', '-y', '--noninteractive', 'flathub', a['flatpak']])
            sh(['flatpak', 'update', '--user', '-y', '--noninteractive', f'--commit={u["commit"]}', a['flatpak']])
            info = sh(['flatpak', 'info', '--user', a['flatpak']]).stdout
            m = re.search(r'Version:\s*(\S+)', info)
            if not m or not m.group(1).startswith(u['from']):
                raise SystemExit(f'the Flathub commit {u["commit"][:12]} is version {m.group(1) if m else "?"}, not {u["from"]}: the pair is not the one of the procedure')
            update_cmd = ['flatpak', 'update', '--user', '-y', '--noninteractive', a['flatpak']]
        else:
            keys = parse_keys(args.keys)
            if not args.pkg_base_url:
                raise SystemExit('panpak: --pkg-base-url is needed (the update comes from the address in the manifest, which is the same host)')
            for what, cmd in install_cmds('panpak', args.app, f'{args.pkg_base_url.rstrip("/")}/{a["panpak_id"]}.prev.panpak', None, keys, previous=True):
                sh(cmd)
            update_cmd = [PANPAK, 'update', '--yes', '--timeout', '300', a['panpak_id']]
        quiet()
        space0, used0, rx0, t0 = space(args.system), fs_used(), rx_bytes(), time.time()
        p = sh(update_cmd)
        wall = time.time() - t0
        quiet()
        rx1, space1, used1 = rx_bytes(), space(args.system), fs_used()
        record(args.system, kind='update', app=args.app, trial=i, from_version=u['from'], to_version=u['to'], wall_s=round(wall, 3), rx_bytes=rx1 - rx0,
               space_bytes=sum(space1.values()) - sum(space0.values()), fs_used_bytes=used1 - used0, said=(p.stdout + p.stderr).strip().splitlines()[-4:],
               reported_bytes=int(m2.group(1)) if (m2 := re.search(r'downloaded bytes: (\d+)', p.stdout + p.stderr)) else None)


def descendants(pid):
    """`pid` and every process below it, from /proc (the tree as the host sees it, whatever the namespaces of the sandbox are)."""
    kids = {}
    for d in os.listdir('/proc'):
        if d.isdigit():
            try:
                ppid = int((Path('/proc') / d / 'stat').read_text().rsplit(')', 1)[1].split()[1])
                kids.setdefault(ppid, []).append(int(d))
            except (OSError, ValueError, IndexError):
                pass
    out, stack = [pid], [pid]
    while stack:
        for c in kids.get(stack.pop(), []):
            out.append(c)
            stack.append(c)
    return out


def proc_facts(pid):
    p = Path('/proc') / str(pid)
    st = dict(line.split(':\t', 1) for line in (p / 'status').read_text().splitlines() if ':\t' in line)
    # The namespaces of this script are the ones of the session (the host side): /proc/1 is not readable by a user.
    host = {n: os.readlink(f'/proc/self/ns/{n}') for n in ('user', 'pid', 'net', 'ipc', 'mnt')}
    ns = {n: os.readlink(p / 'ns' / n) for n in host}
    return {'pid': pid, 'cmd': (p / 'cmdline').read_bytes().replace(b'\0', b' ').decode(errors='replace')[:220], 'seccomp': st.get('Seccomp'), 'no_new_privs': st.get('NoNewPrivs'),
            'cap_eff': st.get('CapEff'), 'shares_with_host': sorted(n for n in ns if host[n] and ns[n] == host[n]), 'user_ns': ns['user'], 'pid_ns': ns['pid']}


def cmd_sandbox(args):
    """How an app that embeds Chromium (CEF: ONLYOFFICE) is sandboxed by the system, seen from outside: the window of the app names its process, and every process below it is read: the filter of system calls,
    `no_new_privs`, the capabilities, which namespaces it shares with the host, and whether its command line has `--no-sandbox` (the app turns off the sandbox of Chromium itself). The same command on the two
    machines gives the two tables that are compared (PROCEDURE.md section 7)."""
    if not extension_ready():
        raise SystemExit('the window extension is not running: PROCEDURE.md, section 1')
    unit = f'panpak-bench-sandbox-{os.getpid()}'
    t0 = time.time()
    proc = subprocess.Popen(['systemd-run', '--user', '--scope', '--quiet', f'--unit={unit}', '--', *launcher(args.system, args.app)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    row = wait_frame_row(args.app, t0, 90)
    if not row:
        stop(args.system, args.app, unit)
        raise SystemExit('no window: nothing to read')
    time.sleep(args.settle)
    facts = []
    for pid in descendants(row['pid']):
        try:
            facts.append(proc_facts(pid))
        except OSError:
            pass
    stop(args.system, args.app, unit)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
    record(args.system, kind='sandbox', app=args.app, window_pid=row['pid'], processes=facts, no_sandbox_flag=sum('--no-sandbox' in f['cmd'] for f in facts), seccomp_modes=sorted({f['seccomp'] for f in facts}))


def cmd_prerequisites(args):
    """The launcher and its prerequisites, measured in every case: `panpak install-runtime <stub> --allow-unsigned` in a private, empty home (nothing of the machine is touched), its time and the space it takes.
    The user should never have to run it by hand (step 10a); here it is only the number that Flatpak's runtimes are put next to. Panpak only."""
    if args.system != 'panpak':
        raise SystemExit('prerequisites: Panpak only (the runtimes of Flatpak are in the install of each app)')
    stub = Path(args.stub or HOME / '.local/bin/panpak-stub')
    for i in range(args.trials):
        home = Path(tempfile.mkdtemp(prefix='panpak-bench-prereq-', dir=HOME / '.cache'))
        try:
            env = dict(os.environ, HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'), XDG_DATA_HOME=str(home / '.local/share'), XDG_CACHE_HOME=str(home / '.cache'))
            drop_caches()
            t0 = time.time()
            p = subprocess.run([PANPAK, 'install-runtime', str(stub), '--allow-unsigned'], env=env, capture_output=True, text=True)
            wall = time.time() - t0
            if p.returncode != 0:
                raise SystemExit(f'install-runtime failed: {p.stdout}{p.stderr}')
            record('panpak', kind='prerequisites', app='-', trial=i, wall_s=round(wall, 3), space_bytes=du(home / '.local/share/panpak'), stub_bytes=stub.stat().st_size)
        finally:
            shutil.rmtree(home, ignore_errors=True)


def cmd_install(args):
    do_install(args.system, args.app, args)


def cmd_start(args):
    warm_starts(args.system, args.app, args.trials, args.gap, args.kind, hold=args.hold)


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


def autologin_on():
    """True if GDM logs the user in by itself (`AutomaticLoginEnable=True` or `AutomaticLogin=` in /etc/gdm/custom.conf)."""
    try:
        text = Path('/etc/gdm/custom.conf').read_text()
    except OSError:
        return False
    return any(re.match(r'(?i)\s*AutomaticLoginEnable\s*=\s*(true|1|yes)', line) for line in text.splitlines())


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
        # After a reboot nobody is at the machine: without the automatic login of GDM the machine stops at the login screen, there is no session, no window extension, and the series never measures anything.
        if os.environ.get('BENCH_SKIP_AUTOLOGIN_CHECK') != '1' and not autologin_on():
            raise SystemExit('refused: the automatic login of GDM is not on (no `AutomaticLogin=true` in /etc/gdm/custom.conf): after the first reboot the machine would wait at the login screen. '
                             'Turn it on (Settings, System, Users, Automatic Login) and run this again (PROCEDURE.md, section 1).')
        apps = args.apps.split(',')
        RESULTS.mkdir(parents=True, exist_ok=True)
        STATE.write_text(json.dumps({'system': args.system, 'apps': apps, 'total': args.count * len(apps), 'done': 0, 'settle': args.settle}))
        UNIT.parent.mkdir(parents=True, exist_ok=True)
        UNIT.write_text(f'[Unit]\nDescription=Measure one start after a reboot\nAfter=graphical-session.target\n\n[Service]\nType=oneshot\n'
                        f'Environment=PYTHONUNBUFFERED=1\nEnvironment=BENCH_RESULTS={RESULTS}\nEnvironment=PATH={HOME}/.local/bin:/usr/local/bin:/usr/bin\nExecStart={sys.executable} {os.path.abspath(__file__)} reboot-series step\n\n[Install]\nWantedBy=default.target\n')
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
    import_session_env()  # the shell has imported its display into the user manager by now
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
        if kind not in ('first', 'warm', 'after-reboot', 'control'):
            continue  # installs, updates and the rest have their own tables
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
            inst.setdefault((r['order'], r['system'], r['app'], ((r.get('tag') or '-').rstrip('0123456789') if (r.get('tag') or '').startswith('cycle') else (r.get('tag') or '-'))), []).append(r)
    for (order, system, app, tag), rs in sorted(inst.items()):
        rx, sp, wall = stats([r['rx_bytes'] for r in rs]), stats([r['space_bytes'] for r in rs]), stats([r['wall_s'] for r in rs])
        say(f'| {order} | {system} | {app} | {tag} | {rx["n"]} | {mib(rx["median"])} | {mib(sp["median"])} | {mib(sp["min"])}-{mib(sp["max"])} | {wall["median"]:.1f} |')
    say('\nThe tag `position0` is the first app of a scenario, `position1` the second: the second one shows what a system shares between two apps. `cycleN` is the Nth reinstall of one app on its own.')
    ups = {}
    for r in rows:
        if r['kind'] == 'update':
            ups.setdefault((r['order'], r['system'], r['app'], f"{r.get('from_version')} to {r.get('to_version')}"), []).append(r)
    if ups:
        say('\n### Updates: bytes that came, space added, time\n')
        say('| order | system | app | versions | n | MiB that came (median) | MiB on disk added (median) | seconds (median) | seconds (min-max) |')
        say('|---|---|---|---|---|---|---|---|---|')
        for (order, system, app, vers), rs in sorted(ups.items()):
            rx, sp, wall = stats([r['rx_bytes'] for r in rs]), stats([r['space_bytes'] for r in rs]), stats([r['wall_s'] for r in rs])
            say(f'| {order} | {system} | {app} | {vers} | {rx["n"]} | {mib(rx["median"])} | {mib(sp["median"])} | {wall["median"]:.1f} | {wall["min"]:.1f}-{wall["max"]:.1f} |')
    pre = [r for r in rows if r['kind'] == 'prerequisites']
    if pre:
        w, sp = stats([r['wall_s'] for r in pre]), stats([r['space_bytes'] for r in pre])
        say('\n### Launcher and prerequisites (`install-runtime` in a private home)\n')
        say(f'n = {w["n"]}, median {w["median"]*1000:.0f} ms, MiB on disk {mib(sp["median"])}. The first `install` of a series also records whether it installed the runtime by itself (`runtime_installed`).')


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
        p.add_argument('--keys', help='panpak: the key of each app as gimp=ed25519:<hex>,... or *=ed25519:<hex> for all (the converter signs every conversion with one key); the install uses --expect-key with it')
        p.add_argument('--ca', help='panpak: the authority of the certificate of the host that serves the packages (https)')

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
    p = sub.add_parser('prerequisites')
    common(p, apps=False)
    p.add_argument('--stub', help='the panpak-stub (default: ~/.local/bin/panpak-stub)')
    p.add_argument('--trials', type=int, default=5)
    p.set_defaults(fn=cmd_prerequisites)
    p = sub.add_parser('sandbox')
    common(p)
    p.add_argument('--settle', type=float, default=8.0, help='seconds to wait after the window, so that the helpers of the browser engine are started')
    p.set_defaults(fn=cmd_sandbox)
    p = sub.add_parser('update')
    common(p)
    pkg(p)
    p.add_argument('--pkg-base-url', help='panpak: the folder that serves <app id>.panpak and <app id>.prev.panpak')
    p.add_argument('--trials', type=int, default=3)
    p.set_defaults(fn=cmd_update)
    p = sub.add_parser('start')
    common(p)
    p.add_argument('--kind', default='warm')
    p.add_argument('--trials', type=int, default=15)
    p.add_argument('--gap', type=float, default=3.0)
    p.add_argument('--hold', type=float, default=0.0, help='seconds to leave the window on the screen after its first frame (for a person who watches; 0 in a series)')
    p.set_defaults(fn=cmd_start)
    p = sub.add_parser('cycle')
    common(p)
    pkg(p)
    p.add_argument('--trials', type=int, default=5)
    p.set_defaults(fn=cmd_cycle)
    p = sub.add_parser('scenario')
    common(p, apps=False)
    p.add_argument('--order', required=True, help='the apps in the order they are installed: gimp,kdenlive,onlyoffice,qb or the reverse (panpak needs --pkg-base-url, --keys and --ca)')
    p.add_argument('--warm', type=int, default=15)
    p.add_argument('--gap', type=float, default=3.0)
    p.add_argument('--pkg-base-url', help='panpak: the folder that serves <app id>.panpak')
    p.add_argument('--keys', help='panpak: gimp=ed25519:<hex>,... or *=ed25519:<hex> (--expect-key)')
    p.add_argument('--ca', help='panpak: the authority of the certificate of the host that serves the packages (https)')
    p.set_defaults(fn=cmd_scenario)
    p = sub.add_parser('reboot-series')
    p.add_argument('action', choices=['start', 'step', 'cancel', 'status'])
    p.add_argument('--system', choices=['flatpak', 'panpak'])
    p.add_argument('--apps', default='gimp,kdenlive,onlyoffice,qb')
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
    import_session_env()
    apply_ca(args)
    args.fn(args)


if __name__ == '__main__':
    main()
