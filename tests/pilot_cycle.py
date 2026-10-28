#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Repeat pilot in an isolated HOME, dedicated Xvfb, without using desktop data.

Usage: python3 tests/pilot_cycle.py package-directory work-directory
HOME is work-directory/home; the script refuses to run unless that is inside the
work directory and is not your real home (see tests/isolation.py).
Rebuild release first. Existing converted payloads retained; only metadata
(test key, X11 for xcb apps) is re-signed. No blanket allow_missing.
"""
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/x86_64-unknown-linux-musl/release'
sys.path.insert(0, str(ROOT / 'tests'))
from incremental import KEY, PUBLIC
import isolation

APPS = ['audacity', 'qbittorrent', 'kdenlive', 'krita', 'darktable',
        'rawtherapee', 'bitwarden', 'joplin', 'openshot', 'mu']


def prepare(source, work):
    home = work / 'home'
    isolation.require_isolated_home(home, work)
    home.mkdir(parents=True, exist_ok=True)
    packages = work / 'packages'
    packages.mkdir(exist_ok=True)
    for app in APPS:
        data = (source / f'org.example.{app}.panpak').read_bytes()
        magic, ver, flags, off, size, moff, mlen, root = struct.unpack('<8sIIQQQQ32s', data[-80:])
        man = data[moff:moff+mlen].decode()
        man = re.sub(r'^key = .*$', f'key = "ed25519:{PUBLIC}"', man, flags=re.M)
        # This pilot explicitly selects X11, including GTK applications.
        # Opt-in is signed into test packages, never a runtime fallback.
        if app in APPS:
            if '[permissions]' in man:
                man = man.replace('[permissions]', '[permissions]\nx11 = true')
            else:
                man += '\n[permissions]\nx11 = true\n'
        man = man.encode()
        pkg = packages / f'{app}.panpak'
        pkg.write_bytes(data[:moff] + man + KEY.sign(man + root)
                        + struct.pack('<8sIIQQQQ32s', magic, ver, flags, off, size, moff, len(man), root))
        env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'))
        p = subprocess.run([BIN / 'panpak', 'install', '--yes', '--runtime', BIN / 'panpak-stub', pkg],
                           env=env, capture_output=True, text=True, timeout=120)
        assert p.returncode == 0, p.stderr
    return home


def environment(home, app, display):
    isolation.require_isolated_home(home, home.parent)
    appdata = home / '.local/share' / f'org.example.{app}'
    appdata.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / '.local/share'),
               XDG_CONFIG_HOME=str(appdata / 'config'), XDG_CACHE_HOME=str(appdata / 'cache'),
               DISPLAY=display, XDG_SESSION_TYPE='x11', GDK_BACKEND='x11',
               QT_QPA_PLATFORM='xcb', XAUTHORITY='', GSETTINGS_BACKEND='memory')
    # sockaddr_un paths are limited to 108 bytes: long work directories
    # otherwise make KIO/QLocalServer fail with "Unknown error 18".
    env['XDG_RUNTIME_DIR'] = isolation.owned_mkdtemp('ngrt-')
    for name in ('config', 'cache'):
        (appdata / name).mkdir(exist_ok=True)
    # Documented path adapters, not new Landlock rules.
    links = {'openshot': ['.openshot_qt'], 'mu': ['.local/share/mu', '.local/share/virtualenv',
             '.local/share/jupyter', '.ipython', 'mu_code'], 'joplin': ['.config/joplin-desktop'],
             'kdenlive': ['.local/share/kdenlive'],
             'qbittorrent': ['.local/share/qBittorrent']}
    for name in links.get(app, []):
        path = home / name
        if not path.exists():
            target = appdata / ('adapter-' + path.name)
            target.mkdir(exist_ok=True)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.symlink_to(target)
    if app == 'krita':
        path = home / '.local/share/krita'
        target = appdata / 'resources'
        target.mkdir(exist_ok=True)
        if not path.exists():
            path.symlink_to(target)
    with (home.parent / 'packages' / f'{app}.panpak').open('rb') as f:
        f.seek(-80, 2)
        root = struct.unpack('<8sIIQQQQ32s', f.read(80))[-1].hex()
    tree = home / '.local/share/panpak/trees' / root
    env['XDG_DATA_DIRS'] = str(tree / 'usr/share') + ':/usr/local/share:/usr/share'
    env['QT_X11_NO_MITSHM'] = '1'
    if app == 'krita':
        env['PYTHONHOME'] = str(tree / 'usr')
    return env


def main():
    source, work = [Path(p).resolve() for p in sys.argv[1:3]]
    work.mkdir(parents=True, exist_ok=True)
    home = prepare(source, work)
    xvfb = shutil.which('Xvfb') or str(Path.home() / 'xvfb/usr/bin/Xvfb')
    fd = os.open(work / 'display', os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o600)
    server = subprocess.Popen([xvfb, '-displayfd', str(fd), '-ac', '-screen', '0', '1280x900x24'],
                              pass_fds=[fd], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    results = {}
    try:
        for _ in range(100):
            text = (work / 'display').read_text().strip()
            if text: break
            time.sleep(0.1)
        assert text, 'no Xvfb'
        display = ':' + text
        (work / 'display-name').write_text(display)
        for app in APPS:
            env = environment(home, app, display)
            flags = ['--no-sandbox', '--disable-gpu'] if app in ('joplin', 'bitwarden') else []
            if app == 'bitwarden': flags += ['--user-data-dir=' + env['XDG_CONFIG_HOME']]
            log = (work / (app + '.log')).open('w')
            p = subprocess.Popen([home / '.local/bin' / f'org.example.{app}', *flags], env=env,
                                 stdout=log, stderr=log, start_new_session=True,
                                 cwd=home / '.local/share' / f'org.example.{app}')
            time.sleep(15)
            windows = subprocess.run(['xwininfo', '-root', '-tree'], env=env,
                                     capture_output=True, text=True).stdout
            results[app] = {'alive': p.poll() is None, 'returncode': p.poll(), 'windows': windows}
            print(app, json.dumps(results[app]), flush=True)
            # Stop the isolated process group (SIGTERM first, SIGKILL only for
            # what is left), never unrelated desktop apps.
            isolation.stop_group(p.pid, p)
            isolation.safe_rmtree(env['XDG_RUNTIME_DIR'], work)
            log.close()
            (work / 'results.json').write_text(json.dumps(results, indent=2))
    finally:
        server.terminate()
        server.wait(timeout=5)
        os.close(fd)


if __name__ == '__main__':
    main()
